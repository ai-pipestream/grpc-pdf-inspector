// SPDX-License-Identifier: Apache-2.0

//! Generated protobuf code for the `ai.pipestream.pdf.v1` package.
//!
//! The files under `src/gen` are produced by `buf generate` (see
//! `buf.gen.yaml`; never edit them by hand). Regenerate after any change under
//! `proto/`:
//!
//! ```sh
//! buf lint
//! buf generate
//! buf build -o src/gen/file_descriptor_set.binpb
//! ```
//!
//! The module below mirrors the protobuf package path exactly, so a sibling
//! package (for example a vendored `ai.pipestream.document.v1`) can be added
//! later without moving anything; [`v1`] is the short name the rest of the
//! crate uses.

/// The protobuf package tree, nested to match the `.proto` package paths.
#[allow(clippy::all, clippy::pedantic, clippy::nursery, missing_docs)]
pub mod ai {
    /// The `ai.pipestream` namespace.
    pub mod pipestream {
        /// The `ai.pipestream.pdf` namespace.
        pub mod pdf {
            /// Messages, enums, client, and server for
            /// `ai.pipestream.pdf.v1`.
            ///
            /// Wire-level documentation lives in the `.proto` files (buf
            /// enforces a comment on every item there); the generated Rust
            /// carries it over where prost supports it.
            pub mod v1 {
                // The prost output already ends with an `include!` of the
                // tonic half, pulling in the client and server modules.
                include!("gen/ai/pipestream/pdf/v1/ai.pipestream.pdf.v1.rs");
            }
        }

        /// The `ai.pipestream.document` namespace.
        pub mod document {
            /// Messages for `ai.pipestream.document.v1`, the Document plane
            /// this service can project a parse into.
            ///
            /// The schema is vendored byte-identical from the gRParse
            /// repository and is never edited here; `document_fold` is the
            /// only code in this crate that builds one. There is no tonic
            /// half: the package declares no services.
            pub mod v1 {
                include!("gen/ai/pipestream/document/v1/ai.pipestream.document.v1.rs");
            }
        }
    }
}

/// The `org.apache.opennlp` protobuf package tree that the Document schema
/// imports: the OpenNLP analyses `Document.analyses` carries, vendored
/// byte-identical from gRParse beside `document.proto`. Nested to match the
/// package path for the same reason as [`ai`]; this crate never fills them.
#[allow(clippy::all, clippy::pedantic, clippy::nursery, missing_docs)]
pub mod org {
    /// The `org.apache` namespace.
    pub mod apache {
        /// The `org.apache.opennlp` namespace.
        pub mod opennlp {
            /// The `org.apache.opennlp.grpc` namespace.
            pub mod grpc {
                /// Messages and enums for `org.apache.opennlp.grpc.v1`.
                pub mod v1 {
                    include!("gen/org/apache/opennlp/grpc/v1/org.apache.opennlp.grpc.v1.rs");
                }
            }
        }
    }
}

/// The `ai.pipestream.pdf.v1` package: this service's own wire contract.
pub use ai::pipestream::pdf::v1;

/// Serialized `FileDescriptorSet` for `proto/ai/pipestream/pdf/v1`, backing
/// gRPC server reflection.
///
/// Codegen runs through `buf generate` rather than a build script, so there is
/// no build-time descriptor set to reuse; this is the `buf build` output
/// checked in next to the generated Rust.
pub const FILE_DESCRIPTOR_SET: &[u8] = include_bytes!("gen/file_descriptor_set.binpb");
