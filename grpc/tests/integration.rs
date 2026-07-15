//! End-to-end: a real tonic gRPC server (generated from greeter.proto) is called
//! by the dynamic client, which knows the service only from the same .proto.

mod pb {
    tonic::include_proto!("demo");
}

use pb::greeter_server::{Greeter, GreeterServer};
use pb::{HelloReply, HelloRequest};
use std::pin::Pin;
use tokio_stream::Stream;
use tonic::{transport::Server, Request, Response, Status};

#[derive(Default)]
struct MyGreeter;

type ReplyStream = Pin<Box<dyn Stream<Item = Result<HelloReply, Status>> + Send>>;

#[tonic::async_trait]
impl Greeter for MyGreeter {
    async fn say_hello(&self, req: Request<HelloRequest>) -> Result<Response<HelloReply>, Status> {
        let name = req.into_inner().name;
        if name == "boom" {
            return Err(Status::invalid_argument("нельзя boom"));
        }
        Ok(Response::new(HelloReply { message: format!("Привет, {name}!") }))
    }

    type SayHelloStreamStream = ReplyStream;

    async fn say_hello_stream(
        &self,
        req: Request<HelloRequest>,
    ) -> Result<Response<Self::SayHelloStreamStream>, Status> {
        let name = req.into_inner().name;
        let s = async_stream::stream! {
            for i in 1..=3 {
                yield Ok(HelloReply { message: format!("{name} #{i}") });
            }
        };
        Ok(Response::new(Box::pin(s)))
    }

    async fn say_hello_client_stream(
        &self,
        req: Request<tonic::Streaming<HelloRequest>>,
    ) -> Result<Response<HelloReply>, Status> {
        let mut stream = req.into_inner();
        let mut names = Vec::new();
        while let Some(msg) = stream.message().await? {
            names.push(msg.name);
        }
        Ok(Response::new(HelloReply {
            message: format!("получено {}: {}", names.len(), names.join(", ")),
        }))
    }

    type SayHelloBidiStream = ReplyStream;

    async fn say_hello_bidi(
        &self,
        req: Request<tonic::Streaming<HelloRequest>>,
    ) -> Result<Response<Self::SayHelloBidiStream>, Status> {
        let mut stream = req.into_inner();
        let s = async_stream::stream! {
            while let Some(msg) = stream.message().await? {
                yield Ok(HelloReply { message: format!("эхо: {}", msg.name) });
            }
        };
        Ok(Response::new(Box::pin(s)))
    }
}

async fn spawn_server() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
    tokio::spawn(async move {
        Server::builder()
            .add_service(GreeterServer::new(MyGreeter))
            .serve_with_incoming(incoming)
            .await
            .unwrap();
    });
    // Give the server a moment to start accepting.
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    format!("http://{addr}")
}

fn proto_path() -> String {
    format!("{}/tests/greeter.proto", env!("CARGO_MANIFEST_DIR"))
}

fn certs_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../testdata/certs")
}

fn read_cert(name: &str) -> Vec<u8> {
    std::fs::read(certs_dir().join(name)).unwrap_or_else(|e| panic!("read {name}: {e}"))
}

fn cert_path(name: &str) -> String {
    certs_dir().join(name).to_string_lossy().into_owned()
}

/// Spawn a TLS-enabled Greeter server on 127.0.0.1, using the throwaway test
/// certs in `testdata/certs` (see that folder's `generate.sh`). With
/// `require_client_cert`, the server also demands a client certificate
/// signed by the same test CA (mTLS).
async fn spawn_tls_server(require_client_cert: bool) -> String {
    use tonic::transport::{Certificate, Identity, ServerTlsConfig};

    let identity = Identity::from_pem(read_cert("server.pem"), read_cert("server.key"));
    let mut tls = ServerTlsConfig::new().identity(identity);
    if require_client_cert {
        tls = tls.client_ca_root(Certificate::from_pem(read_cert("ca.pem")));
    }

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
    tokio::spawn(async move {
        Server::builder()
            .tls_config(tls)
            .expect("valid tls config")
            .add_service(GreeterServer::new(MyGreeter))
            .serve_with_incoming(incoming)
            .await
            .unwrap();
    });
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    format!("https://{addr}")
}

#[tokio::test]
async fn unary_call_roundtrips_via_dynamic_client() {
    let endpoint = spawn_server().await;
    let proto = maelstrom_grpc::Proto::from_file(&proto_path(), &[]).unwrap();

    let res = proto
        .call_json(&endpoint, "demo.Greeter", "SayHello", r#"{"name":"Мир"}"#, 3000)
        .await
        .expect("call ok");
    assert_eq!(res.responses.len(), 1);
    assert!(res.responses[0].contains("Привет, Мир!"), "got {}", res.responses[0]);
    assert!(!res.server_streaming);
}

#[tokio::test]
async fn server_streaming_collects_all_messages() {
    let endpoint = spawn_server().await;
    let proto = maelstrom_grpc::Proto::from_file(&proto_path(), &[]).unwrap();

    let res = proto
        .call_json(&endpoint, "demo.Greeter", "SayHelloStream", r#"{"name":"X"}"#, 3000)
        .await
        .expect("stream ok");
    assert!(res.server_streaming);
    assert_eq!(res.responses.len(), 3);
    assert!(res.responses[2].contains("X #3"));
}

#[tokio::test]
async fn client_streaming_sends_multiple_messages() {
    let endpoint = spawn_server().await;
    let proto = maelstrom_grpc::Proto::from_file(&proto_path(), &[]).unwrap();
    let res = proto
        .call_json(
            &endpoint,
            "demo.Greeter",
            "SayHelloClientStream",
            r#"[{"name":"a"},{"name":"b"},{"name":"c"}]"#,
            3000,
        )
        .await
        .expect("client stream ok");
    assert_eq!(res.responses.len(), 1);
    assert!(res.responses[0].contains("получено 3"), "got {}", res.responses[0]);
}

#[tokio::test]
async fn bidi_streaming_echoes_each() {
    let endpoint = spawn_server().await;
    let proto = maelstrom_grpc::Proto::from_file(&proto_path(), &[]).unwrap();
    let res = proto
        .call_json(
            &endpoint,
            "demo.Greeter",
            "SayHelloBidi",
            r#"[{"name":"x"},{"name":"y"}]"#,
            3000,
        )
        .await
        .expect("bidi ok");
    assert!(res.server_streaming);
    assert_eq!(res.responses.len(), 2);
    assert!(res.responses[0].contains("эхо: x"));
}

#[tokio::test]
async fn error_status_is_reported() {
    let endpoint = spawn_server().await;
    let proto = maelstrom_grpc::Proto::from_file(&proto_path(), &[]).unwrap();
    let err = proto
        .call_json(&endpoint, "demo.Greeter", "SayHello", r#"{"name":"boom"}"#, 3000)
        .await
        .unwrap_err();
    assert!(err.contains("boom") || err.to_lowercase().contains("invalid"), "got {err}");
}

#[tokio::test]
async fn grpc_load_runs_and_aggregates() {
    let endpoint = spawn_server().await;
    let proto = maelstrom_grpc::Proto::from_file(&proto_path(), &[]).unwrap();
    let call = proto
        .build_call(&endpoint, "demo.Greeter", "SayHello", r#"{"name":"load"}"#, 2000)
        .unwrap();

    let cancel = tokio_util::sync::CancellationToken::new();
    let result = maelstrom_grpc::grpc_load(call, 4, 1, None, cancel).await.unwrap();

    assert_eq!(result.method, "gRPC");
    assert!(result.total_requests > 0, "no requests ran");
    assert_eq!(result.errors, 0, "unexpected errors");
    assert!(result.p95_ms >= 0.0);

    // Regression: a successful grpc_load run must show up in the status
    // breakdown as success (HTTP-convention "200"), never as "Сетевая
    // ошибка" — that label is status_label(0, is_db=false), the sentinel
    // for a network/connect failure, not for tonic::Code::Ok.
    assert!(
        !result.status_counts.iter().any(|(label, _)| label == "Сетевая ошибка"),
        "successful gRPC calls mislabeled as network error: {:?}",
        result.status_counts
    );
    let success_count: u64 = result
        .status_counts
        .iter()
        .filter(|(label, _)| label == "200")
        .map(|(_, c)| *c)
        .sum();
    assert_eq!(success_count, result.total_requests, "status breakdown: {:?}", result.status_counts);
}

// ---- g4: gRPC TLS (custom CA / mTLS) ----

#[tokio::test]
async fn tls_call_with_custom_ca_succeeds() {
    let endpoint = spawn_tls_server(false).await;
    let proto = maelstrom_grpc::Proto::from_file(&proto_path(), &[]).unwrap();
    let tls = maelstrom_grpc::TlsConfig {
        ca_cert_pem: Some(cert_path("ca.pem")),
        client_cert_pem: None,
        client_key_pem: None,
        insecure: false,
    };
    let res = proto
        .call_json_with_tls(&endpoint, "demo.Greeter", "SayHello", r#"{"name":"TLS"}"#, 3000, Some(tls))
        .await
        .expect("tls call with trusted CA should succeed");
    assert!(res.responses[0].contains("Привет, TLS!"), "got {}", res.responses[0]);
}

#[tokio::test]
async fn tls_call_without_ca_rejects_selfsigned_cert() {
    // No TLS config at all → native roots only (the existing, unmodified
    // path). The test server's cert is signed by a throwaway CA that isn't
    // in any real trust store, so the handshake must fail — this proves the
    // ca_certificate wiring above is actually load-bearing, and that plain
    // https:// calls still go through the original native-roots-only path.
    let endpoint = spawn_tls_server(false).await;
    let proto = maelstrom_grpc::Proto::from_file(&proto_path(), &[]).unwrap();
    let err = proto
        .call_json(&endpoint, "demo.Greeter", "SayHello", r#"{"name":"X"}"#, 3000)
        .await
        .unwrap_err();
    assert!(!err.is_empty());
}

#[tokio::test]
async fn mtls_call_with_client_identity_succeeds() {
    let endpoint = spawn_tls_server(true).await;
    let proto = maelstrom_grpc::Proto::from_file(&proto_path(), &[]).unwrap();
    let tls = maelstrom_grpc::TlsConfig {
        ca_cert_pem: Some(cert_path("ca.pem")),
        client_cert_pem: Some(cert_path("client.pem")),
        client_key_pem: Some(cert_path("client.key")),
        insecure: false,
    };
    let res = proto
        .call_json_with_tls(&endpoint, "demo.Greeter", "SayHello", r#"{"name":"mTLS"}"#, 3000, Some(tls))
        .await
        .expect("mtls call with a valid client identity should succeed");
    assert!(res.responses[0].contains("Привет, mTLS!"), "got {}", res.responses[0]);
}

#[tokio::test]
async fn mtls_call_without_client_cert_is_rejected() {
    // The server requires a client cert (mTLS); trusting the server's CA
    // without presenting a client identity must still fail the handshake.
    let endpoint = spawn_tls_server(true).await;
    let proto = maelstrom_grpc::Proto::from_file(&proto_path(), &[]).unwrap();
    let tls = maelstrom_grpc::TlsConfig {
        ca_cert_pem: Some(cert_path("ca.pem")),
        client_cert_pem: None,
        client_key_pem: None,
        insecure: false,
    };
    let err = proto
        .call_json_with_tls(&endpoint, "demo.Greeter", "SayHello", r#"{"name":"X"}"#, 3000, Some(tls))
        .await
        .unwrap_err();
    assert!(!err.is_empty());
}
