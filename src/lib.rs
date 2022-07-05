use std::{
    io::{self, Read, Write},
    mem,
    sync::Arc,
    thread,
};

use matchit::*;
use mime::Mime;
use multipart::server::Multipart;
use tiny_http::{HTTPVersion, Header, Request as TinyHttpRequest, Response, StatusCode};

mod err;
pub use err::*;

pub struct MultipartEntry<'v> {
    pub name: Arc<str>,
    pub file_name: Option<String>,
    pub content_type: Option<Mime>,
    pub data: &'v [u8],
}

pub struct Request<'url, 'sender, 'mv> {
    pub url: &'url str,
    pub params: Params<'url, 'url>,
    pub multipart_entry: Option<MultipartEntry<'mv>>,
    pub headers: &'url [Header],
    http_version: HTTPVersion,
    output: &'sender mut (dyn Write + Send + 'static),
}

impl<'url, 'sender, 'mv> Request<'url, 'sender, 'mv> {
    pub fn respond(
        self,
        status: impl Into<StatusCode>,
        headers: Vec<Header>,
        writer: impl FnOnce(&mut dyn Write, &mut io::Empty) -> io::Result<()>,
    ) -> io::Result<()> {
        let response = Response::new(status.into(), headers, io::empty(), None, None);

        TinyHttpRequest::ignore_client_closing_errors(response.print_and_write(
            self.output,
            self.http_version,
            self.headers,
            false,
            None,
            None,
            writer,
        ))
    }

    // i have such good naming
    pub fn respond_with_tinyhttp(self, res: Response<impl Read>) -> io::Result<()> {
        TinyHttpRequest::ignore_client_closing_errors(res.raw_print(
            self.output,
            self.http_version,
            self.headers,
            false,
            None,
        ))
    }
}

pub trait Handler<C: Send + Sync> {
    fn handle<'url, 'sender, 'mv>(
        &self,
        request: Request<'url, 'sender, 'mv>,
        context: C,
    ) -> BeakResult<()>;

    fn needs_multipart(&self) -> bool;

    fn path(&self) -> &'static str;
}

pub fn run<C: Clone + Send + Sync>(
    workers: usize,
    addr: &'static str,
    multipart_upload_limit: usize,
    routes: &'static [&'static (dyn Handler<C> + Send + Sync)],
    context: C,
) -> BeakResult<()> {
    let server = Arc::new(tiny_http::Server::http(addr).expect("Could not bind address"));

    let mut guards = Vec::with_capacity(workers);

    for _ in 0..workers {
        let server = server.clone();
        let context = context.clone();

        let mut router: Router<&(dyn Handler<C> + Send + Sync)> = Router::new();
        for route in routes {
            router.insert(route.path(), *route).unwrap();
        }

        let mut buffer = Vec::with_capacity(multipart_upload_limit);

        let guard = thread::spawn(move || loop {
            let mut mutable_req = server.recv().unwrap();

            // we're going to have to borrow the request both mutably and immutably - we need it's data immutably, and it's output pipe mutably
            // as these don't interact, this is safe to do, but violates borrow rules
            let immutable_req_ptr: *const TinyHttpRequest = &mutable_req;
            let immutable_req = unsafe { immutable_req_ptr.as_ref().unwrap_unchecked() };

            let mut multipart_entry: Option<MultipartEntry<'_>> = None;

            let url = immutable_req.url();
            let matched = router.at(&url).unwrap();

            if matched.value.needs_multipart() {
                if let Some(mut multipart) = Multipart::from_request(&mut mutable_req)
                    .ok()
                    .and_then(|v| v.into_entry().into_result().ok())
                    .flatten()
                {
                    multipart.data.read_to_end(&mut buffer).unwrap();
                    multipart_entry = Some(MultipartEntry {
                        name: multipart.headers.name.clone(),
                        file_name: multipart.headers.filename,
                        content_type: multipart.headers.content_type,
                        data: &buffer,
                    });
                }
            }

            let mut resp_writer = mutable_req.extract_writer_impl();
            let processed_req = Request {
                url: &url,
                params: matched.params,
                multipart_entry,
                headers: immutable_req.headers(),
                http_version: immutable_req.http_version().clone(),
                output: &mut resp_writer,
            };

            matched
                .value
                .handle(processed_req, context.clone())
                .unwrap();

            TinyHttpRequest::ignore_client_closing_errors(resp_writer.flush()).unwrap();

            // destroy our immutable reference *without* running the destructor
            mem::forget(immutable_req);

            // drop our output pipe
            drop(resp_writer);


            if let Some(sender) = mutable_req.notify_when_responded.take() {
                sender.send(()).unwrap();
            }

            // drop our request, running it's destructor

            drop(mutable_req);
        });

        guards.push(guard);
    }

    for guard in guards {
        guard.join().unwrap();
    }

    Ok(())
}

mod macros {
    #[macro_export]
    macro_rules! fn_to_handler {
        ($handler_name:ident with context $ctx:ty; $path:literal => $fn_name:ident with multipart) => {
            pub struct $handler_name;

            impl Handler<$ctx> for $handler_name {
                fn handle<'url, 'sender, 'mv>(
                    &self,
                    request: Request<'url, 'sender, 'mv>,
                    context: $ctx,
                ) -> BeakResult<()> {
                    $fn_name(request, context)
                }
            
                fn needs_multipart(&self) -> bool {
                    true
                }
            
                fn path(&self) -> &'static str {
                    $path
                }
            }
        };

        ($handler_name:ident with context $ctx:ty; $path:literal => $fn_name:ident) => {
            pub struct $handler_name;

            impl Handler<$ctx> for $handler_name {
                fn handle<'url, 'sender, 'mv>(
                    &self,
                    request: Request<'url, 'sender, 'mv>,
                    context: $ctx,
                ) -> BeakResult<()> {
                    $fn_name(request, context)
                }
            
                fn needs_multipart(&self) -> bool {
                    false
                }
            
                fn path(&self) -> &'static str {
                    $path
                }
            }
        };
    }

    macro_rules! parse_base64_hash {
        ($fr:expr) => {
            $fr.and_then(|s| {
                let mut out: [u8; 32] = [0; 32];
                base64_url::decode_to_slice(s, &mut out).ok()?;
                Some(out)
            })
        };
    }

    macro_rules! some_or_response {
        ($opt:expr, or respond to $req:ident with $response:expr) => {
            match $opt {
                Some(v) => v,
                None => {
                    $req.respond_with_tinyhttp($response)?;
                    return Ok(());
                }
            }
        };
    }
}

pub use macros::*;
