use std::{fs, path::Path, time::UNIX_EPOCH};

use objc2::{
    rc::{autoreleasepool, Retained},
    runtime::AnyObject,
    Message,
};
use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep, NSImage, NSWorkspace};
use objc2_foundation::{NSDictionary, NSString};
use objc2_uniform_type_identifiers::{UTType, UTTypeData, UTTypeFolder};

use super::IconQuery;

/// Captures the default application behind a file type. macOS can change the
/// icon shown by Finder after a user changes the default opener, so this value
/// participates in the persistent cache key without leaking the path to logs.
pub(super) fn provider_variant(query: &IconQuery) -> Vec<u8> {
    autoreleasepool(|_| provider_variant_in_pool(query))
}

fn provider_variant_in_pool(query: &IconQuery) -> Vec<u8> {
    let IconQuery::Type { .. } = query else {
        return Vec::new();
    };
    let Some(content_type) = content_type(query) else {
        return Vec::new();
    };

    let workspace = NSWorkspace::sharedWorkspace();
    let mut variant = content_type.identifier().to_string().into_bytes();
    if let Some(application_url) = workspace.URLForApplicationToOpenContentType(&content_type) {
        if let Some(path) = application_url.path() {
            let path = path.to_string();
            variant.extend_from_slice(path.as_bytes());
            append_path_metadata(&mut variant, Path::new(&path));
        }
    }
    variant
}

/// Uses the same AppKit workspace APIs that Finder relies on. Type queries do
/// not require touching a real document, while path queries preserve bundle,
/// volume, and custom-folder icons that cannot safely share an extension key.
pub(super) fn load_png(query: &IconQuery) -> Option<Vec<u8>> {
    autoreleasepool(|_| load_png_in_pool(query))
}

fn load_png_in_pool(query: &IconQuery) -> Option<Vec<u8>> {
    let workspace = NSWorkspace::sharedWorkspace();
    let image = match query {
        IconQuery::Type { .. } => {
            let content_type = content_type(query)?;
            workspace.iconForContentType(&content_type)
        }
        IconQuery::Path { path, .. } => {
            let path = path.to_str()?;
            workspace.iconForFile(&NSString::from_str(path))
        }
    };
    encode_png(&image)
}

fn content_type(query: &IconQuery) -> Option<Retained<UTType>> {
    let IconQuery::Type { key, extension } = query else {
        return None;
    };
    if key == "kind:folder" {
        // SAFETY: UniformTypeIdentifiers exports these immutable, process-wide
        // constants on every supported macOS version.
        return Some(unsafe { UTTypeFolder.retain() });
    }
    let Some(extension) = extension else {
        // SAFETY: See the constant-lifetime explanation above.
        return Some(unsafe { UTTypeData.retain() });
    };
    UTType::typeWithFilenameExtension(&NSString::from_str(extension))
        // SAFETY: See the constant-lifetime explanation above.
        .or_else(|| Some(unsafe { UTTypeData.retain() }))
}

fn encode_png(image: &NSImage) -> Option<Vec<u8>> {
    let tiff = image.TIFFRepresentation()?;
    let bitmap = NSBitmapImageRep::imageRepWithData(&tiff)?;
    let properties = NSDictionary::<objc2_app_kit::NSBitmapImageRepPropertyKey, AnyObject>::new();
    // SAFETY: AppKit accepts an empty property dictionary for PNG output. The
    // generic key/value types match the declaration required by this API.
    let data = unsafe {
        bitmap.representationUsingType_properties(NSBitmapImageFileType::PNG, &properties)
    }?;
    Some(data.to_vec())
}

fn append_path_metadata(variant: &mut Vec<u8>, path: &Path) {
    let Ok(metadata) = fs::metadata(path) else {
        return;
    };
    variant.extend_from_slice(&metadata.len().to_le_bytes());
    if let Ok(modified) = metadata.modified() {
        if let Ok(duration) = modified.duration_since(UNIX_EPOCH) {
            variant.extend_from_slice(&duration.as_secs().to_le_bytes());
            variant.extend_from_slice(&duration.subsec_nanos().to_le_bytes());
        }
    }
}
