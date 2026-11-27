use std::{ffi::c_void, os::windows::ffi::OsStrExt, path::Path, ptr};
use windows::{
    Win32::{
        Foundation::HWND,
        Graphics::Gdi::{
            BI_RGB, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, DeleteObject, GetDC, GetDIBits,
            HBITMAP, HGDIOBJ, ReleaseDC,
        },
        Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW},
    },
    core::{PCWSTR, w},
};

#[derive(Clone)]
pub struct Rgba {
    pub size: u32,
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
        let languages =
            std::slice::from_raw_parts(value.cast::<[u16; 2]>(), len as usize / 4).to_vec();
        for [language, codepage] in languages {
            let query = format!(r"\StringFileInfo\{language:04x}{codepage:04x}\FileDescription");
            let query: Vec<_> = query.encode_utf16().chain([0]).collect();
            if VerQueryValueW(block, PCWSTR(query.as_ptr()), &mut value, &mut len).as_bool()
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

pub fn read_bitmap(bitmap: HBITMAP, size: u32) -> Option<Rgba> {
    let mut bytes = vec![0u8; (size * size * 4) as usize];
    let mut info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: size as i32,
            biHeight: -(size as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    // SAFETY: bitmap is a shell-owned allocation transferred to us. The DIB buffer
    // holds size rows of 32-bit pixels, and the DC and bitmap are released here.
    let rows = unsafe {
        let dc = GetDC(None);
        let rows = if dc.is_invalid() {
            0
        } else {
            let rows = GetDIBits(
                dc,
                bitmap,
                0,
                size,
                Some(bytes.as_mut_ptr().cast()),
                &mut info,
                DIB_RGB_COLORS,
            );
            ReleaseDC(Some(HWND::default()), dc);
            rows
        };
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
        rows
    };
    if rows != size as i32 || bytes.as_chunks::<4>().0.iter().all(|pixel| pixel[3] == 0) {
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
    Some(Rgba { size, bytes })
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
