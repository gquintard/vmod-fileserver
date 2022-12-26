varnish::boilerplate!();

use std::boxed::Box;
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader, Read};
use std::os::raw::{c_uint, c_void};
use std::os::unix::fs::MetadataExt;
use std::ptr;

use anyhow::{anyhow, Result};
use chrono::DateTime;
use chrono::offset::Utc;

use varnish::vcl::ctx::Ctx;
use varnish::vcl::processor::{PullResult, VFPCtx, VFP};

varnish::vtc!(test01);
varnish::vtc!(test02);
varnish::vtc!(test03);
varnish::vtc!(test04);
varnish::vtc!(test05);

// root is the Rust implement of the VCC definition (in vmod.vcc)
// in this vmod, it only holds two raw pointers:
// - be, which is the C varnish oject we can return directly to VCL
// - info_ptr is just here for safekeeping, it's passed to VRT_AddDirector when
//   creating the backend, but we'll need to clean it up when root is dropped
#[allow(non_camel_case_types)]
#[derive(Debug, Clone)]
struct root {
    be: *const varnish_sys::director,
    info_ptr: *mut BackendInfo,
}

// some info that the backend will need when delivering data
#[derive(Debug, Clone)]
struct BackendInfo {
    path: String,                           // top directory of our backend
    mimes: Option<HashMap<String, String>>, // a hashmap linking extensions to maps (optional)
}

// the only thing we care about here is defining a static vdi_methods struct,
// however, because it contains pointers which don't implement the Sync trait,
// we wrap the vdi_methods in MethodWrapper, and implement Sync for this one
// (look for "orphan rule" for more information about this)
struct MethodWrapper {
    methods: varnish_sys::vdi_methods,
}
// we just need a name, the magic number, and the gethdrs and finish functions
// everything else can be null, but we need to be explicit about it
unsafe impl Sync for MethodWrapper {}
static METHODS: MethodWrapper = MethodWrapper{
    methods: varnish_sys::vdi_methods {
        magic: varnish_sys::VDI_METHODS_MAGIC,
        type_: "fileserver\0".as_ptr() as *const std::os::raw::c_char,
        gethdrs: Some(be_gethdrs),
        finish: Some(be_finish),
        destroy: None,
        event: None,
        getip: None,
        healthy: None,
        http1pipe: None,
        list: None,
        panic: None,
        resolve: None,
    }
};

// Rust implementation of the VCC object, it mirrors what happens in C, except
// for a couple of points:
// - we create and return a Rust object, instead of a void pointer
// - new() returns a Result, leaving the error handling to varnish-rs
impl root {
    pub fn new(
        ctx: &mut Ctx,
        vcl_name: &str,
        path: &str,
        mime_db: Option<&str>,
    ) -> Result<Self> {
        // sanity check (note that we don't have null pointers, so path is
        // at worst empty)
        if path.is_empty() {
            // anyhow!() allows us to convert any error to something we can
            // return to varnish-rs
            return Err(anyhow!("fileserver: can't create {} with an empty path", vcl_name));
        }

        // store the mime database in memory, possibly
        let mimes = match mime_db {
            // if there's no path given, we try with a default one, and don't
            // complain if it fails
            None => build_mime_dict("/etc/mime.types").ok(),
            // empty strings means the user does NOT want the mime db
            Some("") => None,
            // otherswise we do want the file to be valid
            Some(p) => Some(build_mime_dict(p)?),
        };

        // create and store our BackendInfo on the heap (Box::new()),
        // and make a pointer to it (Box::into_raw())
        let info_ptr = Box::into_raw(Box::new(
            BackendInfo {
                path: path.to_string(),
                mimes,
            }
            ));

        // call the C function VRT_AddDirector with evertyhing it needs
        let be = unsafe {
            varnish_sys::VRT_AddDirector(
                ctx.raw,
                &METHODS.methods,
                info_ptr as *mut std::ffi::c_void,
                format!("{}\0", vcl_name).as_ptr() as *const i8,
            )
        };

        // yell if it failed, and return a root object
        assert!(!be.is_null());
        Ok(root { be, info_ptr })
    }

    pub fn backend(&self, _ctx: &Ctx) -> *const varnish_sys::director {
        self.be
    }
}

// we normally don't need to handle the _fini() method because Rust is smart
// enough, except that in this case, we need to clean BackendInfo which is
// just a pointer, and we need to delete the director
impl Drop for root {
    fn drop(&mut self) {
        unsafe { 
            varnish_sys::VRT_DelDirector(&mut self.be);
            drop(Box::from_raw(self.info_ptr));
        };
    }
}

// BackendResp will implement the VFP (Varnish Fetch Processor) trait to
// read from a file. This on is extremely dumb because Rust has us covered,
// we just need a BufReader for a file
struct BackendResp {
    reader: std::io::Take<std::io::BufReader<std::fs::File>>,
}
        
// Varnish will give us a buffer and ask us to fill it, and our only work here
// is to pass back how much we wrote, if there was an error, and if we are done
impl VFP for BackendResp {
    fn pull(&mut self, _ctx: &mut VFPCtx, buf: &mut [u8]) -> PullResult {
        if buf.is_empty() {                       
            return PullResult::Ok(0);
        }
        match self.reader.read(buf) {
            Err(_) => PullResult::Err,
            Ok(0) => PullResult::End(0),
            Ok(n) => PullResult::Ok(n),
        }
    }
}

// annoyingly, we create the vfp sctruct manually, mainly because
// we don't have a good way to init the processor here (we do it at the end of
// be_gethdrs instead)
unsafe impl Sync for VfpWrapper {}
struct VfpWrapper {
    vfp: varnish_sys::vfp,
}
static FILE_VFP: VfpWrapper = VfpWrapper {
    vfp: varnish_sys::vfp {
        name: "fileserver\0".as_ptr() as *const i8,
        init: None,
        pull: Some(varnish::vcl::processor::wrap_vfp_pull::<BackendResp>),
        fini: Some(varnish::vcl::processor::wrap_vfp_fini::<BackendResp>),
        priv1: ptr::null(),
    },
};

// we are uninterested by request bodies, so we just read and discard them
unsafe extern "C" fn body_send_iterate(
    _priv_: *mut c_void,
    _flush: c_uint,
    _ptr: *const c_void,
    _l: varnish_sys::ssize_t,
) -> i32 {
    0
}

// short macro to handle Rust failures and bubble them up back to C
// with logging
macro_rules! maybe_fail { 
    ($ctx:expr, $e:expr) => {
        match $e {
            Ok(v) => v,
            Err(e) => {
                (*$ctx.raw.bo).htc = std::ptr::null_mut();
                $ctx.fail(&format!("fileserver: {}", e.to_string()));
                return -1;
            }
        }
    };
}

// implement the C function we said we'd use in METHODS
unsafe extern "C" fn be_gethdrs(
    ctxp: *const varnish_sys::vrt_ctx,
    be: varnish_sys::VCL_BACKEND,
) -> ::std::os::raw::c_int {
    // fluff BackendInfo back into an object (we don't want to modify it as
    // others may be using it concurrently)
    let info = ((*be).priv_ as *const BackendInfo).as_ref().unwrap();
    // create a rust object to avoid dereferencing pointers all the time
    let mut ctx = Ctx::new(ctxp as *mut varnish_sys::vrt_ctx);

    // we know that bereq and bereq_url, so we can just unwrap the options
    let bereq = ctx.http_bereq.as_ref().unwrap();
    let bereq_url = bereq.url().unwrap();

    // mimicking V1F_SendReq in varnish-cache, just to handle the request body
    // TODO: check if we can just get rid of this code and let Varnish handle it
    let bo = ctx.raw.bo.as_mut().unwrap();
    if !bo.bereq_body.is_null() {
        varnish_sys::ObjIterate(bo.wrk, bo.bereq_body, std::ptr::null_mut(), Some(body_send_iterate), 0);
    } else if !bo.req.is_null() && (*bo.req).req_body_status != varnish_sys::BS_NONE.as_ptr() {
        let i = varnish_sys::VRB_Iterate(
            bo.wrk,
            bo.vsl.as_mut_ptr(),
            bo.req,
            Some(body_send_iterate),
            std::ptr::null_mut(),
        );

        if (*bo.req).req_body_status != varnish_sys::BS_CACHED.as_ptr() {
            bo.no_retry = "req.body not cached\0".as_ptr() as *const i8;
        }

        if (*bo.req).req_body_status == varnish_sys::BS_ERROR.as_ptr() {
            assert!(i < 0);
            (*bo.req).doclose = &varnish_sys::SC_RX_BODY[0];
        }
    }

    // combine root and url into something that's hopefully safe
    let path = assemble_file_path(&info.path, bereq_url);

    // FIXME don't try this at home: we should be using `ctx.log()`, but it's
    // already borrowed by bereq
    // varnish::vcl::http::HTTP should probably learn to log too
    Ctx::log(&mut Ctx::new(ctxp as *mut varnish_sys::vrt_ctx), varnish::vcl::ctx::LogTag::Debug, &format!("fileserver: file on disk: {:?}", path));
   
    // let's start building our response
    let beresp = ctx.http_beresp.as_mut().unwrap();

    // open the file and get some metadata
    let f = maybe_fail!(ctx, std::fs::File::open(&path));
    let metadata = maybe_fail!(ctx, f.metadata());
    let cl = metadata.len();
    let modified: DateTime<Utc> = DateTime::from(metadata.modified().unwrap());
    let etag = generate_etag(&metadata);

    // can we avoid sending a body?
    let mut is_304 = false;
    if let Some(inm) = bereq.header("if-none-match") {
        if inm == &etag || (inm.starts_with("W/") && &inm[2..] == &etag) {
            is_304 = true;
        }
    } else if let Some(ims) = bereq.header("if-modified-since") {
        if let Ok(t) = DateTime::parse_from_rfc2822(ims) {
            if t > modified {
                is_304 = true;
            }
        }
    }

    // bo.htc is not really useful to us, but Varnish relies on it for now
    // so we try to play ball
    bo.htc = varnish_sys::WS_Alloc(
        bo.ws.as_mut_ptr(),
        std::mem::size_of::<varnish_sys::http_conn>() as u32,
    ) as *mut varnish_sys::http_conn;
    if bo.htc.is_null() {
        ctx.fail("fileserver: insuficient workspace");
        return -1;
    }
    (*bo.htc).magic = varnish_sys::HTTP_CONN_MAGIC;
    (*bo.htc).doclose = &varnish_sys::SC_REM_CLOSE[0];
    (*bo.htc).content_length = cl as i64;

    maybe_fail!(ctx, beresp.set_proto("HTTP/1.1"));
    if bereq.method() != Some("HEAD") && bereq.method() != Some("GET") {
        // we are fairly strict in what method we accept
        beresp.set_status(405);
        (*bo.htc).body_status = varnish_sys::BS_NONE.as_ptr();
        return 0;
    } else if is_304 {
        // 304 will save us some bandwidth
        beresp.set_status(304);
        (*bo.htc).body_status = varnish_sys::BS_NONE.as_ptr();
    } else {
        // "normal" request, if it's a HEAD to save a bunch of work, but if
        // it's a GET we need to add the VFP to the pipeline
        // and add a BackendResp to the priv1 field
        beresp.set_status(200);
        if bereq.method() == Some("HEAD") {
            (*bo.htc).body_status = varnish_sys::BS_NONE.as_ptr();
        } else {
            (*bo.htc).body_status = varnish_sys::BS_LENGTH.as_ptr();

            let vfe = varnish_sys::VFP_Push(bo.vfc, &FILE_VFP.vfp);
            if vfe.is_null() {
                return -1;
            }

            let backend_resp = BackendResp {
                // prevent reading more than expected
                reader: std::io::BufReader::new(f).take(cl)
            };

            // dropped by wrap_vfp_fini from VfpWrapper
            let respp = Box::into_raw(Box::new(backend_resp));
            (*vfe).priv1 = respp as *mut std::ffi::c_void;
        }
    }

    // set all the headers we can, including the content-type if we can
    maybe_fail!(ctx, beresp.set_header("content-length", &format!("{}", cl)));
    maybe_fail!(ctx, beresp.set_header("etag", &etag));
    maybe_fail!(ctx, beresp.set_header("last-modified", &modified.format("%a, %d %b %Y  %H:%M:%S GMT").to_string()));

    // we only care about content-type if there's content
    if cl > 0 {
        // we need both and extension and a mime database
        if let (Some(ext), Some(h)) = (path.extension(), info.mimes.as_ref()) {
            let ct = h.get(&ext.to_string_lossy() as &str);
            if ct.is_some() {
                maybe_fail!(ctx, beresp.set_header("content-type", &ct.as_ref().unwrap().to_string()));
            }
        }
    }

    // all good, returning 0 means just that
    0
}

// not only does Varnish forces use to create bo.htc, if also forces us to
// clean it. The audacity!
unsafe extern "C" fn be_finish(ctx: *const varnish_sys::vrt_ctx, _arg1: varnish_sys::VCL_BACKEND) {
    (*(*ctx).bo).htc = ptr::null_mut();
}

// reads a mime database into a hashmap, if we can
fn build_mime_dict(path: &str) -> Result<HashMap<String, String>> {
    let mut h = HashMap::new();

    let f = std::fs::File::open(path)?;
    for line in BufReader::new(f).lines() {
        let l = line?;
        let mut ws_it = l.split_whitespace();

        let mime = match ws_it.next() {
            None => continue,
            Some(m) => m,
        };
        // ignore comments
        if mime.chars().next().unwrap_or('-') == '#' {
            continue
        }
        for ext in ws_it {
            if let Some(old_mime) = h.get(ext) {
                return Err(anyhow!("fileserver: error, in {}, extension {} appears to have two types ({} and {})", path, ext, old_mime, mime));
            }
            h.insert(ext.to_string(), mime.to_string());
        }
    }
    Ok(h)
}

#[cfg(test)]
mod build_mime_dict_tests {
    use super::build_mime_dict;

    #[test]
    fn bad() {
        assert_eq!(build_mime_dict("tests/bad1.types").err().unwrap().to_string(), "fileserver: error, in tests/bad1.types, extension txt appears to have two types (application/pdf and application/text)".to_string());
    }

    #[test]
    fn good() {
        let h = build_mime_dict("tests/good1.types").unwrap();
        assert_eq!(h["t1"], "type1");
        assert_eq!(h["T1"], "type1");
        assert_eq!(h["t3"], "type3");
        assert_eq!(h["ty3"], "type3");
        assert_eq!(h["T3"], "type3");
        assert_eq!(h.get("t2"), None);
    }
}

// given root_path and url, assemble the two so that the final path is still
// inside root_path
// There's no access to the file system, and therefore no link resolution
// it can be an issue for multitenancy, beware!
fn assemble_file_path(root_path: &str, url: &str) -> std::path::PathBuf {
    assert_ne!(root_path, "");

    let url_path = std::path::PathBuf::from(url);
    let mut components = Vec::new();

    for c in url_path.components() {
            use std::path::Component::*;
            match c {
                Prefix(_) => unreachable!(),
                RootDir => { },
                CurDir => (),
                ParentDir => { components.pop(); },
                Normal(s) => {
                    // we can unwrap as url_path was created from an &str
                    components.push(s.to_str().unwrap());
                },
            };
    }

    let mut complete_path = String::from(root_path);
    for c in components {
        complete_path.push('/');
        complete_path.push_str(c);
    }
    std::path::PathBuf::from(complete_path)
}

#[cfg(test)]
mod assemble_file_path_tests {
    use super::assemble_file_path;

    fn tc(root_path: &str, url: &str, expected: &str) {
        assert_eq!(assemble_file_path(root_path, url), std::path::PathBuf::from(expected));
    }

    #[test]
    fn simple() { tc("/foo/bar", "/baz/qux", "/foo/bar/baz/qux"); }

    #[test]
    fn simple_slash() { tc("/foo/bar/", "/baz/qux", "/foo/bar/baz/qux"); }

    #[test]
    fn parent() { tc("/foo/bar", "/bar/../qux", "/foo/bar/qux"); }

    #[test]
    fn too_many_parents() { tc("/foo/bar", "/bar/../../qux", "/foo/bar/qux"); }

    #[test]
    fn current() { tc("/foo/bar", "/bar/././qux", "/foo/bar/bar/qux"); }
}

#[derive(Hash)]
struct ShortMd {
    inode: u64,
    size: u64,
    modified: std::time::SystemTime,
}

fn generate_etag(metadata: &std::fs::Metadata) -> String {
    let smd = ShortMd {
        inode: metadata.ino(),
        size: metadata.size(),
        modified: metadata.modified().unwrap(),
    };
    let mut h = DefaultHasher::new();
    smd.hash(&mut h);
    format!("\"{}\"", h.finish())
}


