use anyhow::{Context, Result};
use axum::{
    Router,
    extract::Query,
    http::{StatusCode, header},
    response::{Html, IntoResponse, Redirect},
    routing::get,
};
use std::collections::HashMap;
use tokio::{
    net::TcpListener,
    sync::oneshot,
    task::JoinHandle,
    time::{Duration, sleep},
};

pub struct WebTestServer {
    base_url: String,
    shutdown_tx: Option<oneshot::Sender<()>>,
    handle: JoinHandle<()>,
}

impl WebTestServer {
    pub async fn start() -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .context("failed to bind local web ingest test server")?;
        let address =
            listener.local_addr().context("failed to read local web ingest test server address")?;
        let app = Router::new()
            .route("/seed", get(seed_page))
            .route("/child", get(child_page))
            .route("/canonical", get(canonical_page))
            .route("/redirect", get(redirect_page))
            .route("/recursive/seed", get(recursive_seed_page))
            .route("/recursive/first", get(recursive_first_page))
            .route("/recursive/second", get(recursive_second_page))
            .route("/recursive/depth-two", get(recursive_depth_two_page))
            .route("/recursive/depth-three", get(recursive_depth_three_page))
            .route("/recursive/cycle-a", get(recursive_cycle_a_page))
            .route("/recursive/cycle-b", get(recursive_cycle_b_page))
            .route("/recursive/alias-seed", get(recursive_alias_seed_page))
            .route("/recursive/alias-direct", get(recursive_alias_direct_page))
            .route("/recursive/alias-short", get(recursive_alias_short_redirect))
            .route("/download.txt", get(download_text))
            .route("/unsupported.bin", get(unsupported_binary));
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let handle = tokio::spawn(async move {
            let server = axum::serve(listener, app).with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            });
            let _ = server.await;
        });

        Ok(Self { base_url: format!("http://{address}"), shutdown_tx: Some(shutdown_tx), handle })
    }

    #[must_use]
    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base_url)
    }

    pub async fn shutdown(mut self) -> Result<()> {
        if let Some(shutdown_tx) = self.shutdown_tx.take() {
            let _ = shutdown_tx.send(());
        }
        self.handle.await.context("web ingest test server join failed")?;
        Ok(())
    }
}

async fn seed_page() -> impl IntoResponse {
    Html(
        r#"<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <title>Seed Page</title>
  </head>
  <body>
    <header>
      <nav>
        <a href="/child">Child</a>
        <a href="https://external.example/docs">External</a>
      </nav>
    </header>
    <main>
      <article>
        <h1>Seed Page</h1>
        <p>Canonical single-page ingest should keep only this page by default.</p>
      </article>
    </main>
    <footer>Footer boilerplate</footer>
  </body>
</html>"#,
    )
}

async fn child_page() -> impl IntoResponse {
    Html(
        r#"<!doctype html>
<html lang="en">
  <head><meta charset="utf-8" /><title>Child Page</title></head>
  <body>
    <main>
      <article>
        <h1>Child Page</h1>
        <p>This page exists only to prove single-page ingest does not follow links.</p>
      </article>
    </main>
  </body>
</html>"#,
    )
}

async fn recursive_seed_page(Query(params): Query<HashMap<String, String>>) -> impl IntoResponse {
    if let Some(sleep_ms) = params.get("sleepMs").and_then(|value| value.parse::<u64>().ok()) {
        sleep(Duration::from_millis(sleep_ms)).await;
    }
    let external_link = params
        .get("external")
        .map(|value| format!(r#"<a href="{value}">External allowed</a>"#))
        .unwrap_or_else(|| {
            r#"<a href="https://external.example/docs">External blocked</a>"#.to_string()
        });
    let broken_link = params
        .get("broken")
        .map(|_| r#"<a href="/recursive/missing">Missing</a>"#.to_string())
        .unwrap_or_default();
    let unsupported_link = params
        .get("unsupported")
        .map(|_| r#"<a href="/unsupported.bin">Unsupported</a>"#.to_string())
        .unwrap_or_default();
    Html(format!(
        r#"<!doctype html>
<html lang="en">
  <head><meta charset="utf-8" /><title>Recursive Seed</title></head>
  <body>
    <main>
      <article>
        <h1>Recursive Seed</h1>
        <p>Seed page for recursive crawl coverage.</p>
        <nav>
          <a href="/recursive/first">First</a>
          <a href="/recursive/second">Second</a>
          <a href="/recursive/cycle-a">Cycle A</a>
          {broken_link}
          {unsupported_link}
          {external_link}
        </nav>
      </article>
    </main>
  </body>
</html>"#
    ))
}

async fn recursive_first_page() -> impl IntoResponse {
    Html(
        r#"<!doctype html>
<html lang="en">
  <head><meta charset="utf-8" /><title>Recursive First</title></head>
  <body>
    <main>
      <article>
        <h1>Recursive First</h1>
        <a href="/recursive/depth-two">Depth Two</a>
      </article>
    </main>
  </body>
</html>"#,
    )
}

async fn recursive_second_page() -> impl IntoResponse {
    Html(
        r#"<!doctype html>
<html lang="en">
  <head><meta charset="utf-8" /><title>Recursive Second</title></head>
  <body>
    <main>
      <article>
        <h1>Recursive Second</h1>
        <p>Second same-host page.</p>
      </article>
    </main>
  </body>
</html>"#,
    )
}

async fn recursive_depth_two_page() -> impl IntoResponse {
    Html(
        r#"<!doctype html>
<html lang="en">
  <head><meta charset="utf-8" /><title>Recursive Depth Two</title></head>
  <body>
    <main>
      <article>
        <h1>Recursive Depth Two</h1>
        <a href="/recursive/depth-three">Depth Three</a>
      </article>
    </main>
  </body>
</html>"#,
    )
}

async fn recursive_depth_three_page() -> impl IntoResponse {
    Html(
        r#"<!doctype html>
<html lang="en">
  <head><meta charset="utf-8" /><title>Recursive Depth Three</title></head>
  <body>
    <main>
      <article>
        <h1>Recursive Depth Three</h1>
        <p>Reached default crawl depth.</p>
      </article>
    </main>
  </body>
</html>"#,
    )
}

async fn recursive_cycle_a_page() -> impl IntoResponse {
    Html(
        r#"<!doctype html>
<html lang="en">
  <head><meta charset="utf-8" /><title>Cycle A</title></head>
  <body>
    <main>
      <article>
        <h1>Cycle A</h1>
        <a href="/recursive/cycle-b">Cycle B</a>
      </article>
    </main>
  </body>
</html>"#,
    )
}

async fn recursive_cycle_b_page() -> impl IntoResponse {
    Html(
        r#"<!doctype html>
<html lang="en">
  <head><meta charset="utf-8" /><title>Cycle B</title></head>
  <body>
    <main>
      <article>
        <h1>Cycle B</h1>
        <a href="/recursive/cycle-a">Cycle A</a>
      </article>
    </main>
  </body>
</html>"#,
    )
}

async fn recursive_alias_seed_page() -> impl IntoResponse {
    Html(
        r#"<!doctype html>
<html lang="en">
  <head><meta charset="utf-8" /><title>Alias Seed</title></head>
  <body>
    <main>
      <article>
        <h1>Alias Seed</h1>
        <a href="/recursive/alias-direct">Alias Direct</a>
        <a href="/recursive/alias-short">Alias Short</a>
      </article>
    </main>
  </body>
</html>"#,
    )
}

async fn recursive_alias_direct_page() -> impl IntoResponse {
    Html(
        r#"<!doctype html>
<html lang="en">
  <head><meta charset="utf-8" /><title>Alias Direct</title></head>
  <body>
    <main>
      <article>
        <h1>Alias Direct</h1>
        <p>Canonical target for duplicate redirect coverage.</p>
      </article>
    </main>
  </body>
</html>"#,
    )
}

async fn recursive_alias_short_redirect() -> Redirect {
    Redirect::temporary("/recursive/alias-direct")
}

async fn canonical_page() -> impl IntoResponse {
    Html(
        r#"<!doctype html>
<html lang="en">
  <head><meta charset="utf-8" /><title>Canonical Page</title></head>
  <body>
    <main>
      <article>
        <h1>Canonical Page</h1>
        <p>Redirects should collapse to this final canonical URL.</p>
      </article>
    </main>
  </body>
</html>"#,
    )
}

async fn redirect_page() -> Redirect {
    Redirect::temporary("/canonical")
}

async fn download_text() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        "Direct download text body from the web snapshot.\n",
    )
}

async fn unsupported_binary() -> impl IntoResponse {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/octet-stream")],
        vec![0_u8, 159, 146, 150],
    )
}
