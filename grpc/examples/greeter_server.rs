//! A tiny real gRPC server for manual/e2e testing of the dynamic client and the
//! CLI. Run: `cargo run -p maelstrom-grpc --example greeter_server -- 50055`.

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
        let mut n = 0;
        while let Some(_msg) = stream.message().await? {
            n += 1;
        }
        Ok(Response::new(HelloReply { message: format!("получено {n}") }))
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

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let port: u16 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(50055);
    let addr = format!("127.0.0.1:{port}").parse()?;
    println!("Greeter gRPC на http://{addr}");
    Server::builder()
        .add_service(GreeterServer::new(MyGreeter))
        .serve(addr)
        .await?;
    Ok(())
}
