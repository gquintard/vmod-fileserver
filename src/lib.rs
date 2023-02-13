varnish::boilerplate!();

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::fs::{File, Metadata};
use std::io::{BufRead, BufReader, Read, Take};
use std::os::unix::fs::MetadataExt;

use chrono::DateTime;
use chrono::offset::Utc;

use varnish::vcl::Result;
use varnish::vcl::ctx::Ctx;
use varnish::vcl::backend::{Backend, Serve, Transfer, VCLBackendPtr};

varnish::vtc!(test01);
varnish::vtc!(test02);
varnish::vtc!(test03);
varnish::vtc!(test04);
varnish::vtc!(test05);
varnish::vtc!(test06);

// root is the Rust implement of the VCC definition (in vmod.vcc)
// it only contains backend, which wraps a FileBackend, and
// handles response body creation with a FileTransfer
#[allow(non_camel_case_types)]
struct root {
    backend: Backend<FileBackend, FileTransfer>,
}

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
            return Err(format!("fileserver: can't create {} with an empty path", vcl_name).into());
        }

        // store the mime database in memory, possibly
        let mimes = match mime_db {
            // if there's no path given, we try with a default one, and don't
            // complain if it fails
            None => build_mime_dict("/etc/mime.types").ok(),
            // empty strings means the user does NOT want the mime db
            Some("") => None,
            // otherwise we do want the file to be valid
            Some(p) => Some(build_mime_dict(p)?),
        };

        let backend = Backend::new(ctx, vcl_name,
                                          FileBackend{
                                              mimes,
                                              path: path.to_string()
                                          })?;
        Ok(root { backend })
    }

    pub fn backend(&self, _ctx: &Ctx) -> VCLBackendPtr {
        self.backend.vcl_ptr()
    }
}

struct FileBackend {
    path: String,                           // top directory of our backend
    mimes: Option<HashMap<String, String>>, // a hashmap linking extensions to maps (optional)
}

impl Serve<FileTransfer> for FileBackend<> {
    fn get_type(&self) -> &str {
        "fileserver"
    }

    fn get_headers(&self, ctx: &mut Ctx) -> Result<Option<FileTransfer>> {
        // we know that bereq and bereq_url, so we can just unwrap the options
        let bereq = ctx.http_bereq.as_ref().unwrap();
        let bereq_url = bereq.url().unwrap();

        // combine root and url into something that's hopefully safe
        let path = assemble_file_path(&self.path, bereq_url);
        ctx.log(varnish::vcl::ctx::LogTag::Debug, &format!("fileserver: file on disk: {:?}", path));

        // reset the bereq lifetime, otherwise we couldn't use ctx in the line above
        // yes, it feels weird at first, but it's for our own good
        let bereq = ctx.http_bereq.as_ref().unwrap();

        // let's start building our response
        let beresp = ctx.http_beresp.as_mut().unwrap();

        // open the file and get some metadata
        let f = std::fs::File::open(&path).map_err(|e| e.to_string())?;
        let metadata: Metadata = f.metadata().map_err(|e| e.to_string())?;
        let cl = metadata.len();
        let modified: DateTime<Utc> = DateTime::from(metadata.modified().unwrap());
        let etag = generate_etag(&metadata);

        // can we avoid sending a body?
        let mut is_304 = false;
        if let Some(inm) = bereq.header("if-none-match") {
            if inm == etag || (inm.starts_with("W/") && inm[2..] == etag) {
                is_304 = true;
            }
        } else if let Some(ims) = bereq.header("if-modified-since") {
            if let Ok(t) = DateTime::parse_from_rfc2822(ims) {
                if t > modified {
                    is_304 = true;
                }
            }
        }

        beresp.set_proto("HTTP/1.1")?;
        let mut transfer = None;
        if bereq.method() != Some("HEAD") && bereq.method() != Some("GET") {
            // we are fairly strict in what method we accept
            beresp.set_status(405);
            return Ok(None);
        } else if is_304 {
            // 304 will save us some bandwidth
            beresp.set_status(304);
        } else {
            // "normal" request, if it's a HEAD to save a bunch of work, but if
            // it's a GET we need to add the VFP to the pipeline
            // and add a BackendResp to the priv1 field
            beresp.set_status(200);
            if bereq.method() == Some("GET") {
                transfer = Some(FileTransfer {
                    // prevent reading more than expected
                    reader: std::io::BufReader::new(f).take(cl)
                });
            }
        }

        // set all the headers we can, including the content-type if we can
        beresp.set_header("content-length", &format!("{}", cl))?;
        beresp.set_header("etag", &etag)?;
        beresp.set_header("last-modified", &modified.format("%a, %d %b %Y %H:%M:%S GMT").to_string())?;

        // we only care about content-type if there's content
        if cl > 0 {
            // we need both and extension and a mime database
            if let (Some(ext), Some(h)) = (path.extension(), self.mimes.as_ref()) {
                let ct = h.get(&ext.to_string_lossy() as &str);
                if ct.is_some() {
                    beresp.set_header("content-type", &ct.as_ref().unwrap().to_string())?;
                }
            }
        }
        Ok(transfer)
    }
}

struct FileTransfer {
    reader: Take<BufReader<File>>,
}

impl Transfer for FileTransfer {
    fn len(&self) -> Option<usize> {
        Some(self.reader.limit() as usize)
    }
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        self.reader.read(buf).map_err(|e| e.to_string().into())
    }
}


// reads a mime database into a hashmap, if we can
fn build_mime_dict(path: &str) -> Result<HashMap<String, String>> {
    let mut h = HashMap::new();

    let f = File::open(path).map_err(|e| e.to_string())?;
    for line in BufReader::new(f).lines() {
        let l = line.map_err(|e| e.to_string())?;
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
                return Err(format!("fileserver: error, in {}, extension {} appears to have two types ({} and {})", path, ext, old_mime, mime).into());
            }
            h.insert(ext.to_string(), mime.to_string());
        }
    }
    Ok(h)
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

fn generate_etag(metadata: &std::fs::Metadata) -> String {
    #[derive(Hash)]
    struct ShortMd {
        inode: u64,
        size: u64,
        modified: std::time::SystemTime,
    }

    let smd = ShortMd {
        inode: metadata.ino(),
        size: metadata.size(),
        modified: metadata.modified().unwrap(),
    };
    let mut h = DefaultHasher::new();
    smd.hash(&mut h);
    format!("\"{}\"", h.finish())
}

#[cfg(test)]
mod tests {
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


