//! The `jinn.http` capability: one blocking HTTP request per call.
//!
//! The client is reqwest's blocking mode on the rustls provider `main`
//! installs at startup (a test binary installs its own through the same
//! entry). Redirects follow reqwest's default (up to ten). One client is
//! built per script run, held in the [`Caps`] context.
//!
//! The request takes a table `{method?, url, headers?, body?}` and the
//! response is a table `{status, headers, body}` — headers a name→value
//! table (multi-value headers keep the last value, the ordinary scripting
//! shape), body the raw bytes lossy-UTF8 converted.

use std::rc::Rc;

use mlua::Lua;
use mlua::Table;

use crate::caps::Caps;

/// Registers `jinn.http.request`.
///
/// # Errors
///
/// Propagates any registration failure (harness-side, not script-side).
pub fn inject(lua: &Lua, jinn: &Table, caps: &Rc<Caps>) -> mlua::Result<()> {
    let request_caps = Rc::clone(caps);
    let family = lua.create_table()?;
    family.raw_set(
        "request",
        lua.create_function(move |lua, request: Table| run_request(lua, &request_caps, &request))?,
    )?;
    jinn.raw_set("http", family)?;
    Ok(())
}

/// Runs one request table through the blocking client.
fn run_request(lua: &Lua, caps: &Rc<Caps>, request: &Table) -> mlua::Result<Table> {
    let url: String = request.get("url")?;
    let method: Option<String> = request.get("method").ok();

    let builder = caps
        .http
        .request(method_name(method)?, &url)
        .apply_headers(request.get::<Option<Table>>("headers")?)
        .apply_body(request.get::<Option<String>>("body")?);

    let response = builder
        .send()
        .map_err(|error| mlua::Error::runtime(format!("request to `{url}` failed: {error}")))?;

    let status = response.status().as_u16();

    let headers = lua.create_table()?;
    for (name, value) in response.headers() {
        let Ok(value) = value.to_str() else {
            continue;
        };
        headers.raw_set(name.as_str(), value)?;
    }

    let body = response
        .bytes()
        .map_err(|error| mlua::Error::runtime(format!("cannot read the response body: {error}")))?;

    let result = lua.create_table()?;
    result.raw_set("status", mlua::Integer::from(status))?;
    result.raw_set("headers", headers)?;
    result.raw_set("body", String::from_utf8_lossy(&body).into_owned())?;
    Ok(result)
}

/// The method word, defaulting to GET; a word outside the HTTP method
/// grammar is a script error naming it. (reqwest accepts any token as a
/// method and only fails on the network hop, so the method grammar is
/// validated here — a "NOTAMETHOD" must be rejected at the table, not
/// surface as a connection error.)
fn method_name(method: Option<String>) -> mlua::Result<reqwest::Method> {
    const KNOWN: [&str; 9] = [
        "GET", "HEAD", "POST", "PUT", "DELETE", "OPTIONS", "PATCH", "TRACE", "CONNECT",
    ];
    let Some(word) = method else {
        return Ok(reqwest::Method::GET);
    };
    let upper = word.to_uppercase();
    if KNOWN.contains(&upper.as_str()) {
        // Inside the grammar by construction; an Err here is impossible,
        // but this function returns Result, so map it instead of panicking.
        reqwest::Method::from_bytes(upper.as_bytes())
            .map_err(|error| mlua::Error::runtime(format!("invalid method `{word}`: {error}")))
    } else {
        Err(mlua::Error::runtime(format!(
            "unknown method `{word}`: not one of {KNOWN:?}"
        )))
    }
}

/// A request builder mid-assembly, so each optional field is one named
/// step instead of a nested match.
trait RequestApplying {
    fn apply_headers(self, headers: Option<Table>) -> Self;
    fn apply_body(self, body: Option<String>) -> Self;
}

impl RequestApplying for reqwest::blocking::RequestBuilder {
    fn apply_headers(self, headers: Option<Table>) -> Self {
        let Some(headers) = headers else {
            return self;
        };
        let Ok(pairs): mlua::Result<Vec<(String, String)>> =
            headers.pairs::<String, String>().collect()
        else {
            return self;
        };
        let mut builder = self;
        for (name, value) in pairs {
            builder = builder.header(name, value);
        }
        builder
    }

    fn apply_body(self, body: Option<String>) -> Self {
        match body {
            Some(body) => self.body(body),
            None => self,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unwrap_used,
        reason = "test assertions need expect/panic to state what went wrong"
    )]

    use std::rc::Rc;

    use super::*;
    use crate::caps::Caps;

    /// A Caps carrying a blocking client, against a fresh temp cwd.
    fn http_caps() -> (Rc<Caps>, jinn_testutil::temp::JinnTempDir) {
        let dir = jinn_testutil::tempdir();
        let client = reqwest::blocking::Client::new();
        (
            Rc::new(Caps::with_http(Vec::new(), dir.path.clone(), None, client)),
            dir,
        )
    }

    /// A request table built in the given VM (the capability closure reads
    /// it inside that VM, so the table must belong to it).
    fn request_table(lua: &Lua, method: &str, url: &str) -> Table {
        let table = lua.create_table().expect("table");
        table.raw_set("method", method).expect("method");
        table.raw_set("url", url).expect("url");
        table
    }

    #[rstest::rstest]
    #[test]
    fn request_returns_status_and_body_from_a_mock_server() {
        // Given a mock server answering 200 with a fixed body.
        install_crypto_provider();
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", "/thing")
            .with_status(200)
            .with_body("jinn-http-body")
            .create();
        let (caps, _dir) = http_caps();
        let lua = Lua::new();
        let jinn = lua.create_table().expect("table");
        inject(&lua, &jinn, &caps).expect("injects");
        let request: mlua::Function = jinn
            .get::<mlua::Table>("http")
            .expect("http family")
            .get("request")
            .expect("request");

        // When requesting the mocked path.
        let table = request_table(&lua, "GET", &format!("{}/thing", server.url()));
        let response: Table = request.call(table).expect("request runs");

        // Then the status and body come back.
        let status: mlua::Integer = response.get("status").expect("status");
        let body: String = response.get("body").expect("body");
        assert_eq!(status, 200);
        assert_eq!(body, "jinn-http-body");
        mock.assert();
    }

    #[rstest::rstest]
    #[test]
    fn request_sends_the_declared_method_and_header() {
        // Given a mock server matching POST plus one header.
        install_crypto_provider();
        let mut server = mockito::Server::new();
        let mock = server
            .mock("POST", "/submit")
            .match_header("x-jinn-probe", "yes")
            .with_status(201)
            .create();
        let (caps, _dir) = http_caps();
        let lua = Lua::new();
        let jinn = lua.create_table().expect("table");
        inject(&lua, &jinn, &caps).expect("injects");
        let request: mlua::Function = jinn
            .get::<mlua::Table>("http")
            .expect("http family")
            .get("request")
            .expect("request");

        // When sending a POST with the header. The request table is built
        // in this VM and read back inside the capability closure, so the
        // whole exchange lives and dies with `lua` below.
        let url = format!("{}/submit", server.url());
        let table = request_table(&lua, "POST", &url);
        let headers = lua.create_table().expect("headers");
        headers.raw_set("x-jinn-probe", "yes").expect("header");
        table.raw_set("headers", headers).expect("headers set");
        let response: Table = request.call(table).expect("request runs");
        let status: mlua::Integer = response.get("status").expect("status");
        let code = i32::try_from(status).expect("status fits");
        drop(response);

        // Then the mock matched and reports 201 (asserted after the Lua
        // values are dropped, before the VM goes away).
        std::mem::drop(lua);
        assert_eq!(code, 201);
        mock.assert();
    }

    #[rstest::rstest]
    #[test]
    fn an_unknown_method_is_a_script_error() {
        // Given the http capability.
        install_crypto_provider();
        let (caps, _dir) = http_caps();
        let lua = Lua::new();
        let jinn = lua.create_table().expect("table");
        inject(&lua, &jinn, &caps).expect("injects");
        let request: mlua::Function = jinn
            .get::<mlua::Table>("http")
            .expect("http family")
            .get("request")
            .expect("request");

        // When requesting with a nonsense method.
        let table = request_table(&lua, "NOTAMETHOD", "http://127.0.0.1:1/");
        let result: mlua::Result<Table> = request.call(table);

        // Then the error names the word, without a panic. The error text is
        // copied out before the VM drops (it may hold VM-owned internals).
        let message = result.expect_err("bad method").to_string();
        std::mem::drop(lua);
        assert!(message.contains("NOTAMETHOD"), "got {message}");
    }
}

/// The root crate's crypto-provider installer, reachable from this
/// module's tests.
#[cfg(test)]
use crate::install_crypto_provider;
