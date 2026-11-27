use std::{ffi::c_void, os::windows::ffi::OsStrExt, path::Path, ptr};
use windows::{
    Win32::{
        Foundation::{HWND, SIZE},
        Graphics::Gdi::{
            BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, DeleteObject, GetDC,
            GetDIBits, GetObjectW, HBITMAP, HGDIOBJ, ReleaseDC,
        },
        Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW},
        UI::Shell::{
            IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_ICONONLY, SIIGBF_SCALEUP,
        },
    },
    core::{PCWSTR, w},
};

#[derive(Clone)]
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub bytes: Vec<u8>,
}

impl Rgba {
    pub fn gray(&self) -> Self {
        let mut gray = self.clone();
        for pixel in gray.bytes.as_chunks_mut::<4>().0 {
            let intensity =
                (u16::from(pixel[0]) * 3 + u16::from(pixel[1]) * 6 + u16::from(pixel[2])) / 10;
            pixel[..3].fill(intensity as u8);
        }
        gray
    }
}

pub fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain([0]).collect()
}

pub fn app_icon(path: &Path) -> Option<Rgba> {
    let name = wide(path);
    let size = 56;
    // SAFETY: the icon thread initialized COM, the path is NUL-terminated,
    // and read_bitmap consumes the returned HBITMAP.
    unsafe {
        SHCreateItemFromParsingName::<_, _, IShellItemImageFactory>(PCWSTR(name.as_ptr()), None)
            .and_then(|item| {
                item.GetImage(
                    SIZE { cx: size, cy: size },
                    SIIGBF_ICONONLY | SIIGBF_SCALEUP,
                )
            })
            .ok()
            .and_then(read_bitmap)
    }
}

pub fn file_description(path: &Path) -> Option<String> {
    let path = wide(path);
    let path = PCWSTR(path.as_ptr());
    // SAFETY: path is NUL-terminated. The version block stays alive for all queries.
    unsafe {
        let size = GetFileVersionInfoSizeW(path, None);
        if size == 0 {
            return None;
        }
        let mut storage = vec![0u32; (size as usize).div_ceil(4)];
        let block = storage.as_mut_ptr().cast::<c_void>();
        GetFileVersionInfoW(path, None, size, block).ok()?;
        let mut value = ptr::null_mut();
        let mut len = 0;
        if !VerQueryValueW(block, w!(r"\VarFileInfo\Translation"), &mut value, &mut len).as_bool() {
            return None;
        }
        if value.is_null() || len < 4 {
            return None;
        }
        let languages =
            std::slice::from_raw_parts(value.cast::<[u16; 2]>(), len as usize / 4).to_vec();
        for [language, codepage] in languages {
            let query = format!(r"\StringFileInfo\{language:04x}{codepage:04x}\FileDescription");
            let query: Vec<_> = query.encode_utf16().chain([0]).collect();
            if VerQueryValueW(block, PCWSTR(query.as_ptr()), &mut value, &mut len).as_bool()
                && !value.is_null()
                && len > 0
            {
                let units = std::slice::from_raw_parts(value.cast::<u16>(), len as usize);
                let name = String::from_utf16_lossy(units);
                let name = name.trim_end_matches('\0').trim();
                if !name.is_empty() {
                    return Some(name.into());
                }
            }
        }
        None
    }
}

pub fn read_bitmap(bitmap: HBITMAP) -> Option<Rgba> {
    let object = HGDIOBJ(bitmap.0);
    let mut meta = BITMAP::default();
    // SAFETY: GetImage transferred this bitmap to us. GetObjectW fills a writable BITMAP.
    let size = unsafe {
        GetObjectW(
            object,
            std::mem::size_of::<BITMAP>() as i32,
            Some((&mut meta as *mut BITMAP).cast()),
        )
    };
    // SAFETY: The bitmap came from GetImage, so it is valid until it is freed below.
    unsafe {
        let image = (size != 0 && meta.bmWidth > 0 && meta.bmHeight > 0)
            .then(|| bitmap_rgba(bitmap, meta.bmWidth, meta.bmHeight))
            .flatten();
        let _ = DeleteObject(object);
        image
    }
}

fn bitmap_rgba(bitmap: HBITMAP, width: i32, height: i32) -> Option<Rgba> {
    let mut bytes = vec![0u8; width as usize * height as usize * 4];
    let mut info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    // SAFETY: The bitmap is valid and unselected. The buffer holds height rows of width
    // 32-bit pixels. The screen DC is released on this thread.
    let rows = unsafe {
        let dc = GetDC(None);
        if dc.is_invalid() {
            return None;
        }
        let rows = GetDIBits(
            dc,
            bitmap,
            0,
            height as u32,
            Some(bytes.as_mut_ptr().cast()),
            &mut info,
            DIB_RGB_COLORS,
        );
        ReleaseDC(Some(HWND::default()), dc);
        rows
    };
    if rows != height || bytes.as_chunks::<4>().0.iter().all(|pixel| pixel[3] == 0) {
        return None;
    }
    for pixel in bytes.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
        let alpha = u16::from(pixel[3]);
        if alpha > 0 && alpha < 255 {
            for component in &mut pixel[..3] {
                *component = (u16::from(*component) * 255 / alpha).min(255) as u8;
            }
        }
    }
    Some(Rgba {
        width: width as u32,
        height: height as u32,
        bytes,
    })
}

pub fn accent(icon: &Rgba) -> Option<[u8; 3]> {
    let mut buckets = [(0u32, [0u32; 3]); 64];
    for &[red, green, blue, alpha] in icon.bytes.as_chunks::<4>().0 {
        let max = red.max(green).max(blue);
        let min = red.min(green).min(blue);
        if alpha < 128 || max - min < 45 {
            continue;
        }
        let bucket =
            usize::from(red / 64) * 16 + usize::from(green / 64) * 4 + usize::from(blue / 64);
        let (count, sums) = &mut buckets[bucket];
        *count += 1;
        for (sum, component) in sums.iter_mut().zip([red, green, blue]) {
            *sum += u32::from(component);
        }
    }
    if let Some((count, sums)) = buckets.into_iter().filter(|b| b.0 != 0).max_by_key(|b| b.0) {
        return Some(sums.map(|sum| (sum / count) as u8));
    }
    None
}

pub fn fallback_accent(name: &str) -> [u8; 3] {
    const COLORS: [[u8; 3]; 8] = [
        [88, 101, 242],
        [224, 72, 62],
        [58, 141, 222],
        [47, 163, 127],
        [138, 92, 246],
        [125, 132, 148],
        [178, 89, 150],
        [157, 106, 40],
    ];
    let hash = name.bytes().fold(0usize, |hash, byte| {
        hash.wrapping_mul(31).wrapping_add(usize::from(byte))
    });
    COLORS[hash % COLORS.len()]
}
