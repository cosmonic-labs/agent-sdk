//! A filesystem with nothing in it.
//!
//! `get-directories` returns no preopened directories, and a component can
//! reach a file only through a descriptor that came from one, so no descriptor
//! ever exists and every method below is unreachable in practice. They refuse
//! anyway (`access`), rather than trap, so that a caller probing for a file
//! gets an ordinary error: a trap here would take the whole agent down, which
//! is what stubbing these imports with traps did.
//!
//! Composed in front of the Pi agent (see ../compose.wac): the QuickJS runtime
//! links wasi-libc's filesystem support whether or not the agent ever touches
//! a file, and this satisfies those imports inside the component, so the
//! published artifact imports no `wasi:filesystem` at all.

mod bindings {
    wit_bindgen::generate!({
        path: "wit",
        world: "deny-fs",
        generate_all,
    });
}

use bindings::exports::wasi::filesystem::preopens;
use bindings::exports::wasi::filesystem::types::{
    Advice, Descriptor, DescriptorBorrow, DescriptorFlags, DescriptorStat, DescriptorType,
    DirectoryEntry, DirectoryEntryStream, ErrorCode, Filesize, Guest, GuestDescriptor,
    GuestDirectoryEntryStream, MetadataHashValue, NewTimestamp, OpenFlags, PathFlags,
};
use bindings::wasi::io::error::Error;
use bindings::wasi::io::streams::{InputStream, OutputStream};

type R<T> = Result<T, ErrorCode>;

const NO: ErrorCode = ErrorCode::Access;

struct DenyFs;

/// Never constructed: there is no way to obtain one.
pub struct NoDescriptor;
/// Never constructed: only a descriptor could open one.
pub struct NoEntries;

impl Guest for DenyFs {
    type Descriptor = NoDescriptor;
    type DirectoryEntryStream = NoEntries;

    fn filesystem_error_code(_err: &Error) -> Option<ErrorCode> {
        None
    }
}

impl preopens::Guest for DenyFs {
    fn get_directories() -> Vec<(Descriptor, String)> {
        Vec::new()
    }
}

impl GuestDirectoryEntryStream for NoEntries {
    fn read_directory_entry(&self) -> R<Option<DirectoryEntry>> {
        Err(NO)
    }
}

impl GuestDescriptor for NoDescriptor {
    fn read_via_stream(&self, _offset: Filesize) -> R<InputStream> { Err(NO) }
    fn write_via_stream(&self, _offset: Filesize) -> R<OutputStream> { Err(NO) }
    fn append_via_stream(&self) -> R<OutputStream> { Err(NO) }
    fn advise(&self, _offset: Filesize, _length: Filesize, _advice: Advice) -> R<()> { Err(NO) }
    fn sync_data(&self) -> R<()> { Err(NO) }
    fn get_flags(&self) -> R<DescriptorFlags> { Err(NO) }
    fn get_type(&self) -> R<DescriptorType> { Err(NO) }
    fn set_size(&self, _size: Filesize) -> R<()> { Err(NO) }
    fn set_times(&self, _a: NewTimestamp, _m: NewTimestamp) -> R<()> { Err(NO) }
    fn read(&self, _length: Filesize, _offset: Filesize) -> R<(Vec<u8>, bool)> { Err(NO) }
    fn write(&self, _buffer: Vec<u8>, _offset: Filesize) -> R<Filesize> { Err(NO) }
    fn read_directory(&self) -> R<DirectoryEntryStream> { Err(NO) }
    fn sync(&self) -> R<()> { Err(NO) }
    fn create_directory_at(&self, _path: String) -> R<()> { Err(NO) }
    fn stat(&self) -> R<DescriptorStat> { Err(NO) }
    fn stat_at(&self, _f: PathFlags, _path: String) -> R<DescriptorStat> { Err(NO) }
    fn set_times_at(&self, _f: PathFlags, _path: String, _a: NewTimestamp, _m: NewTimestamp) -> R<()> { Err(NO) }
    fn link_at(&self, _f: PathFlags, _old: String, _to: DescriptorBorrow<'_>, _new: String) -> R<()> { Err(NO) }
    fn open_at(&self, _f: PathFlags, _path: String, _o: OpenFlags, _d: DescriptorFlags) -> R<Descriptor> { Err(NO) }
    fn readlink_at(&self, _path: String) -> R<String> { Err(NO) }
    fn remove_directory_at(&self, _path: String) -> R<()> { Err(NO) }
    fn rename_at(&self, _old: String, _to: DescriptorBorrow<'_>, _new: String) -> R<()> { Err(NO) }
    fn symlink_at(&self, _old: String, _new: String) -> R<()> { Err(NO) }
    fn unlink_file_at(&self, _path: String) -> R<()> { Err(NO) }
    fn is_same_object(&self, _other: DescriptorBorrow<'_>) -> bool { false }
    fn metadata_hash(&self) -> R<MetadataHashValue> { Err(NO) }
    fn metadata_hash_at(&self, _f: PathFlags, _path: String) -> R<MetadataHashValue> { Err(NO) }
}

bindings::export!(DenyFs with_types_in bindings);
