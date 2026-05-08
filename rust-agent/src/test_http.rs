//! Minimal blocking HTTP test server for local integration tests.

use std::io::Read;
use std::io::Write;
use std::net::SocketAddr;
use std::net::TcpListener;
use std::net::TcpStream;
use std::sync::Arc;
use std::sync::Mutex;
use std::thread;

#[derive(Clone, Debug)]
pub(crate) struct TestResponse {
    status: u16,
    body: String,
    content_type: &'static str,
    headers: Vec<(&'static str, String)>,
}

impl TestResponse {
    pub(crate) fn json(status: u16, body: impl Into<String>) -> Self {
        Self {
            status,
            body: body.into(),
            content_type: "application/json",
            headers: Vec::new(),
        }
    }

    pub(crate) fn sse(status: u16, body: impl Into<String>) -> Self {
        Self {
            status,
            body: body.into(),
            content_type: "text/event-stream",
            headers: Vec::new(),
        }
    }

    pub(crate) fn with_header(mut self, name: &'static str, value: impl Into<String>) -> Self {
        self.headers.push((name, value.into()));
        self
    }
}

#[derive(Clone, Debug)]
pub(crate) struct TestRequest {
    pub(crate) path: String,
    pub(crate) headers: String,
    pub(crate) body: String,
}

pub(crate) struct TestServer {
    addr: SocketAddr,
    requests: Arc<Mutex<Vec<TestRequest>>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl TestServer {
    pub(crate) fn new(responses: Vec<TestResponse>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let thread_requests = Arc::clone(&requests);
        let thread = thread::spawn(move || {
            for response in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_request(&mut stream);
                thread_requests.lock().unwrap().push(request);
                write_response(&mut stream, response);
            }
        });
        Self {
            addr,
            requests,
            thread: Some(thread),
        }
    }

    pub(crate) fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub(crate) fn requests(&self) -> Vec<TestRequest> {
        if let Some(thread) = self.thread.as_ref() {
            while !thread.is_finished() {
                std::thread::yield_now();
            }
        }
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        if let Some(thread) = self.thread.take() {
            if thread.is_finished() {
                let _ = thread.join();
            }
        }
    }
}

fn read_request(stream: &mut TcpStream) -> TestRequest {
    let mut bytes = Vec::new();
    let mut buffer = [0; 1024];
    loop {
        let read = stream.read(&mut buffer).unwrap();
        assert_ne!(read, 0, "connection closed before request headers");
        bytes.extend_from_slice(&buffer[..read]);
        if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    let header_end = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .unwrap()
        + 4;
    let headers = String::from_utf8_lossy(&bytes[..header_end]).to_string();
    let content_length = content_length(&headers);
    while bytes.len() < header_end + content_length {
        let read = stream.read(&mut buffer).unwrap();
        assert_ne!(read, 0, "connection closed before request body");
        bytes.extend_from_slice(&buffer[..read]);
    }
    let request_line = headers.lines().next().unwrap();
    let path = request_line.split_whitespace().nth(1).unwrap().to_string();
    let body = String::from_utf8_lossy(&bytes[header_end..header_end + content_length]).to_string();
    TestRequest {
        path,
        headers,
        body,
    }
}

fn content_length(headers: &str) -> usize {
    headers
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(str::to_string)
                .and_then(|value| value.trim().parse::<usize>().ok())
        })
        .unwrap_or(0)
}

fn write_response(stream: &mut TcpStream, response: TestResponse) {
    let reason = if response.status == 200 { "OK" } else { "ERR" };
    write!(
        stream,
        "HTTP/1.1 {} {reason}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n",
        response.status,
        response.content_type,
        response.body.len(),
    )
    .unwrap();
    for (name, value) in response.headers {
        write!(stream, "{name}: {value}\r\n").unwrap();
    }
    write!(stream, "\r\n{}", response.body).unwrap();
}
