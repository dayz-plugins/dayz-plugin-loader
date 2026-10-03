//! Laying an on-disk executable out the way Windows maps it.
//!
//! Addresses in the database are image-relative, and in a PE file the on-disk offset of a
//! section differs from its address in memory. Working on a flat image means the tool scans
//! exactly the bytes the loader will scan at runtime, so a pattern that is unique here is
//! unique there.

use std::path::Path;

use object::read::pe::PeFile64;
use object::{Object, ObjectSection};

/// A mapped-out executable.
///
/// `size_of_image` keeps the PE header's own field name; renaming it would only make it
/// harder to match against the header it came from.
#[allow(clippy::struct_field_names)]
pub struct Image {
    /// The image as the loader sees it, section data at its own address.
    pub bytes: Vec<u8>,
    /// `SizeOfImage` from the optional header.
    pub size_of_image: u32,
    /// PE timestamp, for the build metadata.
    pub timestamp: u32,
    /// Size of the file on disk.
    pub file_size: u64,
}

/// Read a PE file and lay out its sections at their virtual addresses.
///
/// # Errors
/// The file could not be read, is not a 64-bit PE, or declares an image size that is not
/// plausible for an executable.
pub fn load(path: &Path) -> Result<Image, String> {
    let data = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let file_size = data.len() as u64;
    let pe =
        PeFile64::parse(&*data).map_err(|e| format!("{}: not a 64-bit PE: {e}", path.display()))?;
    let headers = pe.nt_headers().optional_header;
    let size_of_image = headers.size_of_image.get(object::LittleEndian);
    // A 4 GiB image is not an executable; refuse rather than try to allocate it.
    if size_of_image == 0 || size_of_image > 0x4000_0000 {
        return Err(format!(
            "{}: implausible image size {size_of_image:#X}",
            path.display()
        ));
    }
    let mut bytes = vec![0u8; size_of_image as usize];
    // Headers sit at the image base, and a few references point into them.
    let header_len = data
        .len()
        .min(headers.size_of_headers.get(object::LittleEndian) as usize);
    bytes[..header_len].copy_from_slice(&data[..header_len]);
    // `Section::address` returns the load address, which includes the image base; the
    // database works in image-relative addresses, so the base comes back off.
    let image_base = headers.image_base.get(object::LittleEndian);
    for section in pe.sections() {
        let Ok(address) = usize::try_from(section.address().saturating_sub(image_base)) else {
            return Err(format!(
                "{}: section address does not fit this host",
                path.display()
            ));
        };
        let Ok(contents) = section.data() else {
            continue;
        };
        let Some(slot) = bytes.get_mut(address..address + contents.len()) else {
            return Err(format!(
                "{}: section {} does not fit the image",
                path.display(),
                section.name().unwrap_or("?")
            ));
        };
        slot.copy_from_slice(contents);
    }
    Ok(Image {
        bytes,
        size_of_image,
        timestamp: pe
            .nt_headers()
            .file_header
            .time_date_stamp
            .get(object::LittleEndian),
        file_size,
    })
}
