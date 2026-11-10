//! 剪贴板截图 OCR：只从剪贴板取图，默认用系统 WinRT `Windows.Media.Ocr` 识别。
//!
//! 为什么这样而不那样：
//! - 不截图、不抓屏、不做选区遮罩：截图交给用户已有的截图工具，本模块只做「读一次剪贴板」。
//! - 默认引擎是系统自带 OCR：零依赖、离线可用、隐私上不出本机；设置页显式安装的
//!   PP-OCRv4 只在用户选择「内置」时参与，失败静默回退这里。
//! - 取图同时覆盖 `CF_DIB` / `CF_DIBV5` / `CF_BITMAP` 与注册格式 `PNG`（截图工具常见组合）；
//!   `CF_DIB` 的调色板/位域交给 GDI 的 `SetDIBitsToDevice` 转成 32bpp BGRA，
//!   不为每种 bpp 手写解码器。
//! - 识别出的文本不在这里做语种判断：直接交给 `SyncCoordinator::accept_clipboard_with_cache`，
//!   与剪贴板文本共用同一套「检测 → 池内落卡 → 翻译」规则，避免两套语种逻辑漂移。
//! - `AvailableRecognizerLanguages` 为空或系统没有可用识别器时静默返回 `None`：
//!   项目已移除全部 notice，缺语种不弹任何提示。
//! - 识别器优先用户配置语言，失败再退到任一可用识别语言；两者都没有就整次放弃。

#[cfg(not(windows))]
pub struct Bitmap {
    pub bgra: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

#[cfg(not(windows))]
pub fn clipboard_bitmap() -> Option<Bitmap> {
    None
}

#[cfg(not(windows))]
pub fn recognize_bitmap_system(_bitmap: &Bitmap, _language_tag: Option<&str>) -> Option<String> {
    None
}

/// 剪贴板里是否有可识别的图像格式（只看格式，不解码、不 OCR）。
/// 热键路径先做这一步，避免每次取词都白跑一次 OCR。
#[cfg(not(windows))]
pub fn clipboard_has_image() -> bool {
    false
}

#[cfg(windows)]
pub use imp::Bitmap;

#[cfg(windows)]
pub fn clipboard_bitmap() -> Option<Bitmap> {
    imp::clipboard_bitmap()
}

#[cfg(windows)]
pub fn recognize_bitmap_system(bitmap: &Bitmap, language_tag: Option<&str>) -> Option<String> {
    imp::recognize_bitmap_system(bitmap, language_tag)
}

#[cfg(windows)]
pub fn clipboard_has_image() -> bool {
    imp::clipboard_has_image()
}

#[cfg(windows)]
mod imp {
    use std::mem::size_of;

    use windows::core::{HSTRING, Interface};
    use windows::Globalization::Language;
    use windows::Graphics::Imaging::{
        BitmapAlphaMode, BitmapDecoder, BitmapInterpolationMode, BitmapPixelFormat, BitmapTransform,
        ColorManagementMode, ExifOrientationMode, SoftwareBitmap,
    };
    use windows::Media::Ocr::OcrEngine;
    use windows::Storage::Streams::{
        Buffer, DataReader, DataWriter, IOutputStream, IRandomAccessStream, InMemoryRandomAccessStream,
    };
    use windows::Win32::Foundation::HGLOBAL;
    use windows::Win32::Graphics::Gdi::{
        CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDIBits, GetObjectW,
        SelectObject, SetDIBitsToDevice, BITMAP, BITMAPINFO, BITMAPINFOHEADER, BI_BITFIELDS,
        BI_JPEG, BI_PNG, BI_RGB, DIB_RGB_COLORS, HBITMAP, HGDIOBJ,
    };
    use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};
    use windows::Win32::System::DataExchange::{
        CloseClipboard, EnumClipboardFormats, GetClipboardData, GetClipboardFormatNameW,
        OpenClipboard,
    };
    use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};

    const CF_BITMAP: u32 = 2;
    const CF_DIB: u32 = 8;
    const CF_DIBV5: u32 = 17;
    /// 图片像素上限：超过就放弃，避免一张超大图把 OCR 的内存与耗时放大。
    const MAX_IMAGE_SIDE: i32 = 8192;
    /// Windows OCR 对短边过小的图会直接返回空（实测 98×25 原图空、2× 后成功）。
    /// 短边不足 64px 时按整数倍放大，最多 4 倍。
    const MIN_OCR_SIDE: i32 = 64;
    const MAX_OCR_SCALE: i32 = 4;

    /// 剪贴板里拿到的原始图像。
    enum Image {
        /// PNG/JPEG 等已编码字节，交给 WinRT `BitmapDecoder` 解码。
        Encoded(Vec<u8>),
        /// 未压缩 DIB（BGRA8，顶朝下）。
        Bgra(Vec<u8>, i32, i32),
    }

    /// 剪贴板图像（BGRA8，顶朝下）：内置 OCR 与系统 OCR 共用的最小表示。
    pub struct Bitmap {
        pub bgra: Vec<u8>,
        pub width: u32,
        pub height: u32,
    }

    /// 读剪贴板并解码为 BGRA。不做小图放大：系统 OCR 自己在 `recognize_bitmap_system` 里放大，
    /// 内置引擎的检测网络也会把长边缩放到 960。
    pub fn clipboard_bitmap() -> Option<Bitmap> {
        let _com = ComGuard::new();
        match read_clipboard_image()? {
            Image::Bgra(bytes, width, height) => {
                let expected = (width as usize).checked_mul(height as usize)?.checked_mul(4)?;
                if width <= 0 || height <= 0 || bytes.len() < expected {
                    None
                } else {
                    Some(Bitmap {
                        bgra: bytes[..expected].to_vec(),
                        width: width as u32,
                        height: height as u32,
                    })
                }
            }
            Image::Encoded(bytes) => {
                let bitmap = decode_encoded(&bytes, 1)?;
                software_bitmap_to_bgra(&bitmap)
            }
        }
    }

    /// 系统 OCR：`language_tag` 为 `None` 时用用户配置语言 / 任一可用语言。
    pub fn recognize_bitmap_system(bitmap: &Bitmap, language_tag: Option<&str>) -> Option<String> {
        let _com = ComGuard::new();
        let (pixels, width, height) =
            scaled_bgra(&bitmap.bgra, bitmap.width as i32, bitmap.height as i32)?;
        let software = bgra_to_software_bitmap(&pixels, width, height)?;
        let engine = match language_tag {
            Some(tag) => create_engine_for_language(tag)?,
            None => create_engine()?,
        };
        recognize_bitmap(&software, &engine)
    }

    /// WinRT 调用需要线程已初始化 COM；嵌套调用各自配对，不会多减一次。
    struct ComGuard(bool);

    impl ComGuard {
        fn new() -> Self {
            Self(unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).is_ok() })
        }
    }

    impl Drop for ComGuard {
        fn drop(&mut self) {
            if self.0 {
                unsafe { CoUninitialize() };
            }
        }
    }

    pub fn clipboard_has_image() -> bool {
        if open_clipboard().is_err() {
            return false;
        }
        let mut found = false;
        let mut format = 0u32;
        loop {
            format = unsafe { EnumClipboardFormats(format) };
            if format == 0 {
                break;
            }
            if matches!(format, CF_DIB | CF_DIBV5 | CF_BITMAP) {
                found = true;
                break;
            }
            if format >= 0xC000 && is_png_format_name(format) {
                found = true;
                break;
            }
        }
        let _ = unsafe { CloseClipboard() };
        found
    }

    /// 在剪贴板打开期间把需要的句柄/字节读出来，随后立即关闭剪贴板再跑 OCR。
    fn read_clipboard_image() -> Option<Image> {
        if open_clipboard().is_err() {
            return None;
        }
        let result = unsafe { read_clipboard_image_open() };
        let _ = unsafe { CloseClipboard() };
        result
    }

    unsafe fn read_clipboard_image_open() -> Option<Image> {
        let mut png: Option<Vec<u8>> = None;
        let mut dib: Option<(u32, Vec<u8>)> = None;
        let mut bitmap: Option<HBITMAP> = None;
        let mut format = 0u32;
        loop {
            format = unsafe { EnumClipboardFormats(format) };
            if format == 0 {
                break;
            }
            match format {
                CF_DIBV5 => {
                    if dib.as_ref().is_none_or(|(existing, _)| *existing != CF_DIBV5) {
                        if let Some(bytes) = unsafe { format_bytes(format) } {
                            dib = Some((format, bytes));
                        }
                    }
                }
                CF_DIB => {
                    if dib.is_none() {
                        if let Some(bytes) = unsafe { format_bytes(format) } {
                            dib = Some((format, bytes));
                        }
                    }
                }
                CF_BITMAP => {
                    if bitmap.is_none() {
                        if let Ok(handle) = unsafe { GetClipboardData(CF_BITMAP) } {
                            if !handle.is_invalid() {
                                bitmap = Some(HBITMAP(handle.0));
                            }
                        }
                    }
                }
                registered if registered >= 0xC000 => {
                    if png.is_none() && is_png_format_name(registered) {
                        png = unsafe { format_bytes(registered) };
                    }
                }
                _ => {}
            }
        }
        // 优先 PNG（无损、无调色板）；其次 DIB/DIBV5；最后才是 HBITMAP。
        if let Some(bytes) = png {
            return Some(Image::Encoded(bytes));
        }
        if let Some((_, bytes)) = dib {
            if let Some(layout) = dib_layout(&bytes) {
                let encoded = matches!(layout.compression, c if c == BI_PNG.0 || c == BI_JPEG.0);
                return if encoded {
                    let data = layout.encoded_bytes(&bytes)?;
                    Some(Image::Encoded(data.to_vec()))
                } else {
                    let (pixels, width, height) = unsafe { dib_to_bgra(&bytes, &layout)? };
                    Some(Image::Bgra(pixels, width, height))
                };
            }
            return None;
        }
        if let Some(hbitmap) = bitmap {
            let (pixels, width, height) = unsafe { hbitmap_to_bgra(hbitmap)? };
            return Some(Image::Bgra(pixels, width, height));
        }
        None
    }

    fn open_clipboard() -> Result<(), ()> {
        for _ in 0..8 {
            if unsafe { OpenClipboard(None) }.is_ok() {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(6));
        }
        Err(())
    }

    unsafe fn format_bytes(format: u32) -> Option<Vec<u8>> {
        let handle = unsafe { GetClipboardData(format) }.ok()?;
        if handle.is_invalid() {
            return None;
        }
        let memory = HGLOBAL(handle.0);
        let size = unsafe { GlobalSize(memory) };
        if size == 0 {
            return None;
        }
        let pointer = unsafe { GlobalLock(memory) };
        if pointer.is_null() {
            return None;
        }
        let bytes = unsafe { std::slice::from_raw_parts(pointer.cast::<u8>(), size) }.to_vec();
        let _ = unsafe { GlobalUnlock(memory) };
        Some(bytes)
    }

    fn is_png_format_name(format: u32) -> bool {
        let mut buffer = [0u16; 128];
        let length = unsafe { GetClipboardFormatNameW(format, &mut buffer) };
        if length <= 0 {
            return false;
        }
        let name = String::from_utf16_lossy(&buffer[..length as usize]);
        let name = name.trim().to_ascii_lowercase();
        name == "png" || name == "image/png"
    }

    /// DIB 头解析结果：像素数据偏移与编码方式，足够交给 GDI 或 WinRT 解码。
    struct DibLayout {
        bits_offset: usize,
        width: i32,
        height: i32,
        compression: u32,
        encoded_size: Option<usize>,
    }

    impl DibLayout {
        fn encoded_bytes<'a>(&self, bytes: &'a [u8]) -> Option<&'a [u8]> {
            let end = match self.encoded_size {
                Some(size) => self.bits_offset.checked_add(size)?,
                None => bytes.len(),
            };
            bytes.get(self.bits_offset..end.min(bytes.len()))
        }
    }

    fn dib_layout(bytes: &[u8]) -> Option<DibLayout> {
        let header_size = u32_at(bytes, 0)? as usize;
        if header_size < 40 || header_size > bytes.len() {
            return None;
        }
        let width = i32_at(bytes, 4)?;
        let height = i32_at(bytes, 8)?;
        if width <= 0 || height == 0 || width > MAX_IMAGE_SIDE {
            return None;
        }
        let height_abs = height.unsigned_abs() as i32;
        if height_abs > MAX_IMAGE_SIDE {
            return None;
        }
        let bit_count = u16_at(bytes, 14)?;
        let compression = u32_at(bytes, 16)?;
        let size_image = u32_at(bytes, 20)?;
        let clr_used = u32_at(bytes, 32)?;

        let bits_offset = if matches!(compression, c if c == BI_JPEG.0 || c == BI_PNG.0) {
            header_size
        } else {
            if !matches!(compression, c if c == BI_RGB.0 || c == BI_BITFIELDS.0) {
                return None;
            }
            let palette_entries = if bit_count <= 8 {
                if clr_used > 0 {
                    clr_used as usize
                } else {
                    1usize.checked_shl(bit_count as u32)?
                }
            } else {
                0
            };
            // BITMAPINFOHEADER(40) + BI_BITFIELDS 的 3 个掩码是单独跟在头后面的。
            let masks = if compression == BI_BITFIELDS.0 && header_size == 40 {
                12
            } else {
                0
            };
            header_size
                .checked_add(palette_entries.checked_mul(4)?)?
                .checked_add(masks)?
        };
        if bits_offset > bytes.len() {
            return None;
        }
        Some(DibLayout {
            bits_offset,
            width,
            height,
            compression,
            encoded_size: (size_image > 0).then_some(size_image as usize),
        })
    }

    fn u16_at(bytes: &[u8], offset: usize) -> Option<u16> {
        Some(u16::from_le_bytes(
            bytes.get(offset..offset + 2)?.try_into().ok()?,
        ))
    }

    fn u32_at(bytes: &[u8], offset: usize) -> Option<u32> {
        Some(u32::from_le_bytes(
            bytes.get(offset..offset + 4)?.try_into().ok()?,
        ))
    }

    fn i32_at(bytes: &[u8], offset: usize) -> Option<i32> {
        Some(i32::from_le_bytes(
            bytes.get(offset..offset + 4)?.try_into().ok()?,
        ))
    }

    /// DIB → 32bpp 顶朝下 BGRA：借 GDI 处理调色板、位域与 1/4/8/16/24/32 bpp 的差异。
    unsafe fn dib_to_bgra(bytes: &[u8], layout: &DibLayout) -> Option<(Vec<u8>, i32, i32)> {
        let width = layout.width;
        let height = layout.height.unsigned_abs() as i32;
        let destination = empty_bitmap_info(width as u32, height as u32);
        let hdc = unsafe { CreateCompatibleDC(None) };
        if hdc.is_invalid() {
            return None;
        }
        let mut pixels: *mut std::ffi::c_void = std::ptr::null_mut();
        let hbitmap = unsafe {
            CreateDIBSection(
                Some(hdc),
                &destination,
                DIB_RGB_COLORS,
                &mut pixels,
                None,
                0,
            )
        }
        .ok();
        let Some(hbitmap) = hbitmap else {
            let _ = unsafe { DeleteDC(hdc) };
            return None;
        };
        let previous = unsafe { SelectObject(hdc, HGDIOBJ(hbitmap.0)) };
        let source_info = bytes.as_ptr().cast::<BITMAPINFO>();
        let source_pixels = unsafe { bytes.as_ptr().add(layout.bits_offset) }.cast::<std::ffi::c_void>();
        let drawn = unsafe {
            SetDIBitsToDevice(
                hdc,
                0,
                0,
                width as u32,
                height as u32,
                0,
                0,
                0,
                height as u32,
                source_pixels,
                source_info,
                DIB_RGB_COLORS,
            )
        };
        let output = if drawn == 0 || pixels.is_null() {
            None
        } else {
            let length = (width as usize).checked_mul(height as usize)?.checked_mul(4)?;
            Some(unsafe { std::slice::from_raw_parts(pixels.cast::<u8>(), length) }.to_vec())
        };
        let _ = unsafe { SelectObject(hdc, previous) };
        let _ = unsafe { DeleteObject(HGDIOBJ(hbitmap.0)) };
        let _ = unsafe { DeleteDC(hdc) };
        output.and_then(|pixels| {
            Some((
                pixels,
                width,
                height,
            ))
        })
    }

    unsafe fn hbitmap_to_bgra(hbitmap: HBITMAP) -> Option<(Vec<u8>, i32, i32)> {
        let mut bitmap = BITMAP::default();
        let copied = unsafe {
            GetObjectW(
                HGDIOBJ(hbitmap.0),
                size_of::<BITMAP>() as i32,
                Some((&mut bitmap as *mut BITMAP).cast::<std::ffi::c_void>()),
            )
        };
        if copied == 0 {
            return None;
        }
        let width = bitmap.bmWidth;
        let height = bitmap.bmHeight;
        if width <= 0 || height <= 0 || width > MAX_IMAGE_SIDE || height > MAX_IMAGE_SIDE {
            return None;
        }
        let mut info = empty_bitmap_info(width as u32, height as u32);
        let mut pixels = vec![0u8; (width as usize).checked_mul(height as usize)?.checked_mul(4)?];
        let hdc = unsafe { CreateCompatibleDC(None) };
        if hdc.is_invalid() {
            return None;
        }
        let lines = unsafe {
            GetDIBits(
                hdc,
                hbitmap,
                0,
                height as u32,
                Some(pixels.as_mut_ptr().cast::<std::ffi::c_void>()),
                &mut info,
                DIB_RGB_COLORS,
            )
        };
        let _ = unsafe { DeleteDC(hdc) };
        if lines == 0 {
            return None;
        }
        Some((pixels, width, height))
    }

    fn empty_bitmap_info(width: u32, height: u32) -> BITMAPINFO {
        let mut info = BITMAPINFO::default();
        info.bmiHeader = BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width as i32,
            // 负数 = 顶朝下，和 `SoftwareBitmap` 的 BGRA8 行序一致。
            biHeight: -(height as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        };
        info
    }

    /// 短边不足时需要的整数放大倍数；已在可识别尺寸内则返回 1。
    fn ocr_scale(width: i32, height: i32) -> i32 {
        let min_side = width.min(height).max(1);
        if min_side >= MIN_OCR_SIDE {
            return 1;
        }
        let needed = (MIN_OCR_SIDE + min_side - 1) / min_side;
        let mut scale = needed.clamp(2, MAX_OCR_SCALE);
        while scale > 1
            && (width.saturating_mul(scale) > MAX_IMAGE_SIDE
                || height.saturating_mul(scale) > MAX_IMAGE_SIDE)
        {
            scale -= 1;
        }
        scale
    }

    /// BGRA 走最近邻整数放大：保持字形边缘清晰，OCR 不依赖插值平滑。
    /// 已经是正常尺寸时原样返回。
    fn scaled_bgra(bytes: &[u8], width: i32, height: i32) -> Option<(Vec<u8>, i32, i32)> {
        if width <= 0 || height <= 0 {
            return None;
        }
        let expected = (width as usize).checked_mul(height as usize)?.checked_mul(4)?;
        if bytes.len() < expected {
            return None;
        }
        let scale = ocr_scale(width, height);
        if scale <= 1 {
            return Some((bytes[..expected].to_vec(), width, height));
        }
        let scaled_width = width.checked_mul(scale)?;
        let scaled_height = height.checked_mul(scale)?;
        let mut output = vec![0u8; (scaled_width as usize) * (scaled_height as usize) * 4];
        for y in 0..scaled_height {
            let source_y = (y / scale) as usize;
            for x in 0..scaled_width {
                let source_x = (x / scale) as usize;
                let source_index = (source_y * width as usize + source_x) * 4;
                let target_index = (y as usize * scaled_width as usize + x as usize) * 4;
                output[target_index..target_index + 4]
                    .copy_from_slice(&bytes[source_index..source_index + 4]);
            }
        }
        Some((output, scaled_width, scaled_height))
    }

    fn bgra_to_software_bitmap(bytes: &[u8], width: i32, height: i32) -> Option<SoftwareBitmap> {
        let writer = DataWriter::new().ok()?;
        writer.WriteBytes(bytes).ok()?;
        let buffer = writer.DetachBuffer().ok()?;
        let bitmap = SoftwareBitmap::CreateCopyWithAlphaFromBuffer(
            &buffer,
            BitmapPixelFormat::Bgra8,
            width,
            height,
            BitmapAlphaMode::Ignore,
        )
        .ok()?;
        Some(bitmap)
    }

    /// 编码图统一解码成 BGRA8；放大倍数由调用方给（系统 OCR 需要小图放大，内置引擎给 1）。
    fn decode_encoded(bytes: &[u8], scale: i32) -> Option<SoftwareBitmap> {
        let stream = InMemoryRandomAccessStream::new().ok()?;
        let output: IOutputStream = stream.cast().ok()?;
        let writer = DataWriter::CreateDataWriter(&output).ok()?;
        writer.WriteBytes(bytes).ok()?;
        writer.StoreAsync().ok()?.join().ok()?;
        let _ = writer.DetachStream();
        let _ = stream.Seek(0);
        let random_access: IRandomAccessStream = stream.cast().ok()?;
        let decoder = BitmapDecoder::CreateAsync(&random_access).ok()?.join().ok()?;
        let width = decoder.PixelWidth().ok()?.max(1);
        let height = decoder.PixelHeight().ok()?.max(1);
        let scale = scale.max(1);
        // 编码图不自己解像素：交给 WinRT 在解码阶段按目标尺寸缩放。
        let transform = BitmapTransform::new().ok()?;
        transform.SetScaledWidth(width * scale as u32).ok()?;
        transform.SetScaledHeight(height * scale as u32).ok()?;
        transform
            .SetInterpolationMode(BitmapInterpolationMode::Cubic)
            .ok()?;
        decoder
            .GetSoftwareBitmapTransformedAsync(
                BitmapPixelFormat::Bgra8,
                BitmapAlphaMode::Ignore,
                &transform,
                ExifOrientationMode::IgnoreExifOrientation,
                ColorManagementMode::DoNotColorManage,
            )
            .ok()?
            .join()
            .ok()
    }

    /// Bgra8 `SoftwareBitmap` → 连续 BGRA 字节。
    fn software_bitmap_to_bgra(bitmap: &SoftwareBitmap) -> Option<Bitmap> {
        let width = bitmap.PixelWidth().ok()?;
        let height = bitmap.PixelHeight().ok()?;
        let length = (width as usize).checked_mul(height as usize)?.checked_mul(4)?;
        let buffer = Buffer::Create(u32::try_from(length).ok()?).ok()?;
        bitmap.CopyToBuffer(&buffer).ok()?;
        let reader = DataReader::FromBuffer(&buffer).ok()?;
        let mut pixels = vec![0u8; length];
        reader.ReadBytes(&mut pixels).ok()?;
        Some(Bitmap {
            bgra: pixels,
            width: width as u32,
            height: height as u32,
        })
    }

    fn recognize_bitmap(bitmap: &SoftwareBitmap, engine: &OcrEngine) -> Option<String> {
        let result = engine.RecognizeAsync(bitmap).ok()?.join().ok()?;
        let text = result.Text().ok()?.to_string();
        let text = text.trim().to_string();
        if text.is_empty() {
            None
        } else {
            Some(text)
        }
    }

    /// 调用方指定语言：只在这一个语言族里找识别器，不跨语言回退。
    /// 系统真实标签可能带区域后缀（如 `zh-Hans-CN`），所以先精确匹配，再按前缀匹配。
    fn create_engine_for_language(language_tag: &str) -> Option<OcrEngine> {
        let family = language_tag
            .split('-')
            .next()
            .unwrap_or(language_tag)
            .to_ascii_lowercase();
        if let Ok(available) = OcrEngine::AvailableRecognizerLanguages() {
            if let Ok(count) = available.Size() {
                let mut family_match: Option<Language> = None;
                for index in 0..count {
                    let Ok(language) = available.GetAt(index) else {
                        continue;
                    };
                    let tag = language
                        .LanguageTag()
                        .map(|value| value.to_string())
                        .unwrap_or_default()
                        .to_ascii_lowercase();
                    if tag == language_tag.to_ascii_lowercase() {
                        if let Ok(engine) = OcrEngine::TryCreateFromLanguage(&language) {
                            return Some(engine);
                        }
                    }
                    if family_match.is_none() && tag.starts_with(&format!("{family}-")) {
                        family_match = Some(language);
                    }
                }
                if let Some(language) = family_match {
                    if let Ok(engine) = OcrEngine::TryCreateFromLanguage(&language) {
                        return Some(engine);
                    }
                }
                return None;
            }
        }
        let language = Language::CreateLanguage(&HSTRING::from(language_tag)).ok()?;
        OcrEngine::TryCreateFromLanguage(&language).ok()
    }

    /// 系统 OCR 识别器：优先用户配置语言；没有就任取一个可用语言；都没有则返回 `None`
    /// （不写提示、不降级到别的引擎）。
    fn create_engine() -> Option<OcrEngine> {
        let available = OcrEngine::AvailableRecognizerLanguages().ok()?;
        let count = available.Size().ok()?;
        if count == 0 {
            return None;
        }
        if let Ok(engine) = OcrEngine::TryCreateFromUserProfileLanguages() {
            return Some(engine);
        }
        for index in 0..count {
            if let Ok(language) = available.GetAt(index) {
                if let Ok(engine) = OcrEngine::TryCreateFromLanguage(&language) {
                    return Some(engine);
                }
            }
        }
        None
    }

    #[allow(dead_code)]
    fn available_language_tags() -> Vec<String> {
        let Ok(available) = OcrEngine::AvailableRecognizerLanguages() else {
            return Vec::new();
        };
        let Ok(count) = available.Size() else {
            return Vec::new();
        };
        let mut tags = Vec::new();
        for index in 0..count {
            if let Ok(language) = available.GetAt(index) {
                if let Ok(tag) = language.LanguageTag() {
                    tags.push(tag.to_string());
                }
            }
        }
        tags
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn header(width: i32, height: i32, bit_count: u16, compression: u32, clr_used: u32) -> Vec<u8> {
            let mut bytes = vec![0u8; 40];
            bytes[0..4].copy_from_slice(&40u32.to_le_bytes());
            bytes[4..8].copy_from_slice(&width.to_le_bytes());
            bytes[8..12].copy_from_slice(&height.to_le_bytes());
            bytes[12..14].copy_from_slice(&1u16.to_le_bytes());
            bytes[14..16].copy_from_slice(&bit_count.to_le_bytes());
            bytes[16..20].copy_from_slice(&compression.to_le_bytes());
            bytes[32..36].copy_from_slice(&clr_used.to_le_bytes());
            bytes
        }

        #[test]
        fn a_24_bit_dib_starts_pixels_right_after_the_header() {
            let bytes = header(4, 3, 24, BI_RGB.0, 0);
            let layout = dib_layout(&bytes).expect("layout");
            assert_eq!(layout.width, 4);
            assert_eq!(layout.height, 3);
            assert_eq!(layout.bits_offset, 40);
            assert_eq!(layout.compression, BI_RGB.0);
        }

        #[test]
        fn an_8_bit_dib_reserves_palette_space_before_pixels() {
            let mut bytes = header(2, 2, 8, BI_RGB.0, 0);
            bytes.resize(40 + 256 * 4, 0);
            let layout = dib_layout(&bytes).expect("layout");
            assert_eq!(layout.bits_offset, 40 + 256 * 4);
        }

        #[test]
        fn a_bitfield_dib_adds_three_masks_after_the_header() {
            let mut bytes = header(2, 2, 16, BI_BITFIELDS.0, 0);
            bytes.resize(40 + 12, 0);
            let layout = dib_layout(&bytes).expect("layout");
            assert_eq!(layout.bits_offset, 40 + 12);
        }

        #[test]
        fn a_png_compressed_dib_treats_the_remaining_bytes_as_encoded_data() {
            let mut bytes = header(2, 2, 24, BI_PNG.0, 0);
            bytes[20..24].copy_from_slice(&5u32.to_le_bytes());
            bytes.extend_from_slice(b"png!!");
            let layout = dib_layout(&bytes).expect("layout");
            assert_eq!(layout.encoded_bytes(&bytes).expect("encoded"), b"png!!");
        }

        #[test]
        fn a_compressed_or_absurd_header_is_rejected() {
            let compressed = header(2, 2, 24, 1, 0);
            assert!(dib_layout(&compressed).is_none());
            let too_wide = header(MAX_IMAGE_SIDE + 1, 2, 24, BI_RGB.0, 0);
            assert!(dib_layout(&too_wide).is_none());
            let truncated = vec![0u8; 10];
            assert!(dib_layout(&truncated).is_none());
        }

        #[test]
        fn small_images_get_upscaled_before_ocr() {
            // 实测 98×25 的清晰小图原生尺寸识别为空，放大后才能出字。
            assert_eq!(ocr_scale(98, 25), 3);
            assert_eq!(ocr_scale(400, 100), 1);
            assert_eq!(ocr_scale(1, 1), MAX_OCR_SCALE);
            // 放大后不能越过单边上限：3000×30 先要 3 倍，收回到 2 倍。
            assert_eq!(ocr_scale(3000, 30), 2);
        }

        #[test]
        fn scaled_bgra_uses_nearest_neighbor() {
            let pixels = vec![
                10, 20, 30, 255, // 第一个源像素
                40, 50, 60, 255, // 第二个源像素
            ];
            let (scaled, width, height) = scaled_bgra(&pixels, 2, 1).expect("scaled");
            assert_eq!((width, height), (8, 4));
            assert_eq!(scaled.len(), 8 * 4 * 4);
            assert_eq!(&scaled[0..4], &[10, 20, 30, 255]);
            assert_eq!(&scaled[4 * 4..4 * 4 + 4], &[40, 50, 60, 255]);
        }
    }
}
