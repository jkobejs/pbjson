//! `pbjson-build` consumes the descriptor output of [`prost-build`][1] and generates
//! [`serde::Serialize`][2] and [`serde::Deserialize`][3] implementations
//! that are compliant with the [protobuf JSON mapping][4]
//!
//! # Usage
//!
//! _It is recommended you first follow the example in [prost-build][1] to familiarise
//! yourself with `prost`_
//!
//! Add `prost-build`, `prost`, `pbjson`, `pbjson-build` and `pbjson-types` to
//! your `Cargo.toml`
//!
//! ```toml
//! [dependencies]
//! prost = <prost-version>
//! pbjson = <pbjson-version>
//! pbjson-types = <pbjson-version>
//!
//! [build-dependencies]
//! prost-build = <prost-version>
//! pbjson-build = <pbjson-version>
//! ```
//!
//! Next create a `build.rs` containing the following
//!
//! ```ignore
//! // This assumes protobuf files are under a directory called `protos`
//! // and in a protobuf package `mypackage`
//!
//! let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("protos");
//! let proto_files = vec![root.join("myproto.proto")];
//!
//! // Tell cargo to recompile if any of these proto files are changed
//! for proto_file in &proto_files {
//!     println!("cargo:rerun-if-changed={}", proto_file.display());
//! }
//!
//! let descriptor_path = PathBuf::from(env::var("OUT_DIR").unwrap())
//!     .join("proto_descriptor.bin");
//!
//! prost_build::Config::new()
//!     // Save descriptors to file
//!     .file_descriptor_set_path(&descriptor_path)
//!     // Override prost-types with pbjson-types
//!     .compile_well_known_types()
//!     .extern_path(".google.protobuf", "::pbjson_types")
//!     // Generate prost structs
//!     .compile_protos(&proto_files, &[root])?;
//!
//! let descriptor_set = std::fs::read(descriptor_path)?;
//! pbjson_build::Builder::new()
//!     .register_descriptors(&descriptor_set)?
//!     .build(&[".mypackage"])?;
//! ```
//!
//! Finally within `lib.rs`
//!
//! ```ignore
//! /// Generated by [`prost-build`]
//! include!(concat!(env!("OUT_DIR"), "/mypackage.rs"));
//! /// Generated by [`pbjson-build`]
//! include!(concat!(env!("OUT_DIR"), "/mypackage.serde.rs"));
//! ```
//!
//! The module will now contain the generated prost structs for your protobuf definition
//! along with compliant implementations of [serde::Serialize][2] and [serde::Deserialize][3]
//!
//! [1]: https://docs.rs/prost-build
//! [2]: https://docs.rs/serde/1.0.130/serde/trait.Serialize.html
//! [3]: https://docs.rs/serde/1.0.130/serde/trait.Deserialize.html
//! [4]: https://developers.google.com/protocol-buffers/docs/proto3#json

#![deny(rustdoc::broken_intra_doc_links, rustdoc::bare_urls, rust_2018_idioms)]
#![warn(
    missing_debug_implementations,
    clippy::explicit_iter_loop,
    clippy::use_self,
    clippy::clone_on_ref_ptr,
    clippy::future_not_send
)]

use prost_types::FileDescriptorProto;
use std::collections::HashSet;
use std::io::{BufWriter, Error, ErrorKind, Result, Write};
use std::path::PathBuf;

use crate::descriptor::{Descriptor, Package};
use crate::message::resolve_message;
use crate::{
    generator::{generate_enum, generate_message},
    resolver::Resolver,
};

mod descriptor;
mod escape;
mod generator;
mod message;
mod resolver;

#[derive(Debug, Default)]
pub struct Builder {
    descriptors: descriptor::DescriptorSet,
    exclude: Vec<String>,
    out_dir: Option<PathBuf>,
    extern_paths: Vec<(String, String)>,
    retain_enum_prefix: bool,
    ignore_unknown_fields: bool,
    btree_map_paths: Vec<String>,
    emit_fields: bool,
    emit_enum_fields: bool,
    emit_repeated: bool,
    emit_empty_string: bool,
    use_integers_for_enums: bool,
    preserve_proto_field_names: bool,
    strip_enum_vairant_prefix_and_to_lowercase: bool,
    enum_prefixes_to_keep: HashSet<String>,
}

impl Builder {
    /// Create a new `Builder`
    pub fn new() -> Self {
        Self::default()
    }

    /// Configures the output directory where generated Rust files will be written.
    ///
    /// If unset, defaults to the `OUT_DIR` environment variable. `OUT_DIR` is set by Cargo when
    /// executing build scripts, so `out_dir` typically does not need to be configured.
    pub fn out_dir<P>(&mut self, path: P) -> &mut Self
    where
        P: Into<PathBuf>,
    {
        self.out_dir = Some(path.into());
        self
    }

    /// Register an encoded `FileDescriptorSet` with this `Builder`
    pub fn register_descriptors(&mut self, descriptors: &[u8]) -> Result<&mut Self> {
        self.descriptors.register_encoded(descriptors)?;
        Ok(self)
    }

    /// Register a decoded `FileDescriptor` with this `Builder`
    pub fn register_file_descriptor(&mut self, file: FileDescriptorProto) -> &mut Self {
        self.descriptors.register_file_descriptor(file);
        self
    }

    /// Don't generate code for the following type prefixes
    pub fn exclude<S: Into<String>, I: IntoIterator<Item = S>>(
        &mut self,
        prefixes: I,
    ) -> &mut Self {
        self.exclude.extend(prefixes.into_iter().map(Into::into));
        self
    }

    /// Configures the code generator to not strip the enum name from variant names.
    pub fn retain_enum_prefix(&mut self) -> &mut Self {
        self.retain_enum_prefix = true;
        self
    }

    /// Declare an externally provided Protobuf package or type
    pub fn extern_path(
        &mut self,
        proto_path: impl Into<String>,
        rust_path: impl Into<String>,
    ) -> &mut Self {
        self.extern_paths
            .push((proto_path.into(), rust_path.into()));
        self
    }

    /// Don't error out in the presence of unknown fields when deserializing,
    /// instead skip the field.
    pub fn ignore_unknown_fields(&mut self) -> &mut Self {
        self.ignore_unknown_fields = true;

        self
    }

    /// Generate Rust BTreeMap implementations for Protobuf map type fields.
    pub fn btree_map<S: Into<String>, I: IntoIterator<Item = S>>(&mut self, paths: I) -> &mut Self {
        self.btree_map_paths
            .extend(paths.into_iter().map(Into::into));
        self
    }

    /// Output fields with their default values.
    pub fn emit_fields(&mut self) -> &mut Self {
        self.emit_fields = true;
        self
    }

    /// Output enum fields with their default values.
    pub fn emit_enum_fields(&mut self) -> &mut Self {
        self.emit_enum_fields = true;
        self
    }

    // Output repeated fields if empty.
    pub fn emit_repeated(&mut self) -> &mut Self {
        self.emit_repeated = true;
        self
    }

    // Output empty strings if empty.
    pub fn emit_empty_string(&mut self) -> &mut Self {
        self.emit_empty_string = true;
        self
    }

    // print integers instead of enum names.
    pub fn use_integers_for_enums(&mut self) -> &mut Self {
        self.use_integers_for_enums = true;
        self
    }

    /// Output fields with their original names as defined in their proto schemas, instead of
    /// lowerCamelCase
    pub fn preserve_proto_field_names(&mut self) -> &mut Self {
        self.preserve_proto_field_names = true;
        self
    }

    /// Serde enums to snake lowercase and strip prefix
    pub fn strip_enum_vairant_prefix_and_to_lowercase(&mut self) -> &mut Self {
        self.strip_enum_vairant_prefix_and_to_lowercase = true;
        self
    }

    pub fn enum_prefixes_to_keep<S: Into<String>, I: IntoIterator<Item = S>>(
        &mut self,
        prefixes: I,
    ) -> &mut Self {
        self.enum_prefixes_to_keep
            .extend(prefixes.into_iter().map(Into::into));
        self
    }

    /// Generates code for all registered types where `prefixes` contains a prefix of
    /// the fully-qualified path of the type
    pub fn build<S: AsRef<str>>(&mut self, prefixes: &[S]) -> Result<()> {
        let mut output: PathBuf = self.out_dir.clone().map(Ok).unwrap_or_else(|| {
            std::env::var_os("OUT_DIR")
                .ok_or_else(|| {
                    Error::new(ErrorKind::Other, "OUT_DIR environment variable is not set")
                })
                .map(Into::into)
        })?;
        output.push("FILENAME");

        let write_factory = move |package: &Package| {
            output.set_file_name(format!("{}.serde.rs", package));

            let file = std::fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .create(true)
                .open(&output)?;

            Ok(BufWriter::new(file))
        };

        let writers = self.generate(prefixes, write_factory)?;
        for (_, mut writer) in writers {
            writer.flush()?;
        }

        Ok(())
    }

    /// Generates code into instances of write as provided by the `write_factory`
    ///
    /// This function is intended for use when writing output of code generation
    /// directly to output files is not desired. For most use cases inside a
    /// `build.rs` file, the [`build()`][Self::build] method should be preferred.
    pub fn generate<S: AsRef<str>, W: Write, F: FnMut(&Package) -> Result<W>>(
        &self,
        prefixes: &[S],
        mut write_factory: F,
    ) -> Result<Vec<(Package, W)>> {
        let iter = self.descriptors.iter().filter(move |(t, _)| {
            let exclude = self
                .exclude
                .iter()
                .any(|prefix| t.prefix_match(prefix.as_ref()).is_some());
            let include = prefixes
                .iter()
                .any(|prefix| t.prefix_match(prefix.as_ref()).is_some());
            include && !exclude
        });

        // Exploit the fact descriptors is ordered to group together types from the same package
        let mut ret: Vec<(Package, W)> = Vec::new();
        for (type_path, descriptor) in iter {
            let writer = match ret.last_mut() {
                Some((package, writer)) if package == type_path.package() => writer,
                _ => {
                    let package = type_path.package();
                    ret.push((package.clone(), write_factory(package)?));
                    &mut ret.last_mut().unwrap().1
                }
            };

            let resolver = Resolver::new(
                &self.extern_paths,
                type_path.package(),
                self.retain_enum_prefix,
                self.strip_enum_vairant_prefix_and_to_lowercase,
                &self.enum_prefixes_to_keep,
            );

            match descriptor {
                Descriptor::Enum(descriptor) => generate_enum(
                    &resolver,
                    type_path,
                    descriptor,
                    writer,
                    self.use_integers_for_enums,
                )?,
                Descriptor::Message(descriptor) => {
                    if let Some(message) = resolve_message(&self.descriptors, descriptor) {
                        generate_message(
                            &resolver,
                            &message,
                            writer,
                            self.ignore_unknown_fields,
                            &self.btree_map_paths,
                            self.emit_fields,
                            self.emit_enum_fields,
                            self.emit_repeated,
                            self.emit_empty_string,
                            self.preserve_proto_field_names,
                        )?
                    }
                }
            }
        }

        Ok(ret)
    }
}
