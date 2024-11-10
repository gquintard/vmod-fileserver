vcl 4.1;

import candletest from "/tmp/libvmod_candletest.so";

backend be none;

sub vcl_init {
	new ai = candletest.root();
}

sub vcl_recv {
	set req.backend_hint = ai.backend();
	return(pass);
}
