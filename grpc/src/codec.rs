//! A tonic Codec that speaks `prost_reflect::DynamicMessage`, so we can call any
//! gRPC method known only at runtime (from a compiled `.proto`) without codegen.

use bytes::Buf;
use prost::Message;
use prost_reflect::{DynamicMessage, MessageDescriptor};
use tonic::codec::{Codec, DecodeBuf, Decoder, EncodeBuf, Encoder};
use tonic::Status;

/// Codec for a single method: encodes the request message and decodes responses
/// into the method's output message type.
pub struct DynCodec {
    pub output: MessageDescriptor,
}

impl Codec for DynCodec {
    type Encode = DynamicMessage;
    type Decode = DynamicMessage;
    type Encoder = DynEncoder;
    type Decoder = DynDecoder;

    fn encoder(&mut self) -> Self::Encoder {
        DynEncoder
    }
    fn decoder(&mut self) -> Self::Decoder {
        DynDecoder { output: self.output.clone() }
    }
}

pub struct DynEncoder;

impl Encoder for DynEncoder {
    type Item = DynamicMessage;
    type Error = Status;

    fn encode(&mut self, item: Self::Item, dst: &mut EncodeBuf<'_>) -> Result<(), Self::Error> {
        item.encode(dst).map_err(|e| Status::internal(format!("кодирование: {e}")))
    }
}

pub struct DynDecoder {
    output: MessageDescriptor,
}

impl Decoder for DynDecoder {
    type Item = DynamicMessage;
    type Error = Status;

    fn decode(&mut self, src: &mut DecodeBuf<'_>) -> Result<Option<Self::Item>, Self::Error> {
        // DecodeBuf covers exactly one length-delimited message frame.
        let remaining = src.remaining();
        let mut msg = DynamicMessage::new(self.output.clone());
        msg.merge(src.take(remaining))
            .map_err(|e| Status::internal(format!("декодирование: {e}")))?;
        Ok(Some(msg))
    }
}
