use std::convert::TryFrom;
use std::io::Read;
use std::path;

#[rustfmt::skip]
use ::image;

use crate::context::{Context, DebugId};
use crate::error::GameError;
use crate::error::GameResult;
use crate::filesystem;
use crate::graphics;
use crate::graphics::shader::*;
use crate::graphics::*;

/// Generic in-GPU-memory image data available to be drawn on the screen.
/// You probably just want to look at the `Image` type.
#[derive(Clone, PartialEq)]
pub struct ImageGeneric<B>
where
    B: BackendSpec,
{
    pub(crate) texture: gfx::handle::RawShaderResourceView<B::Resources>,
    pub(crate) texture_handle: gfx::handle::RawTexture<B::Resources>,
    pub(crate) sampler_info: gfx::texture::SamplerInfo,
    pub(crate) blend_mode: Option<BlendMode>,
    pub(crate) width: u16,
    pub(crate) height: u16,

    pub(crate) debug_id: DebugId,
}

impl<B> ImageGeneric<B>
where
    B: BackendSpec,
{
    /// A helper function that just takes a factory directly so we can make an image
    /// without needing the full context object, so we can create an Image while still
    /// creating the GraphicsContext.
    pub(crate) fn make_raw(
        factory: &mut <B as BackendSpec>::Factory,
        sampler_info: &texture::SamplerInfo,
        width: u16,
        height: u16,
        rgba: &[u8],
        color_format: gfx::format::Format,
        debug_id: DebugId,
    ) -> GameResult<Self> {
        if width == 0 || height == 0 {
            let msg = format!(
                "Tried to create a texture of size {}x{}, each dimension must
                be >0",
                width, height
            );
            return Err(GameError::ResourceLoadError(msg));
        }
        // Check for overflow, which might happen on 32-bit systems.
        // Textures can be max u16*u16, pixels, but then have 4 bytes per pixel.
        let uwidth = usize::from(width);
        let uheight = usize::from(height);
        let expected_bytes = uwidth
            .checked_mul(uheight)
            .and_then(|size| size.checked_mul(4))
            .ok_or_else(|| {
                let msg = format!(
                    "Integer overflow in Image::make_raw, image size: {} {}",
                    uwidth, uheight
                );
                GameError::ResourceLoadError(msg)
            })?;
        if expected_bytes != rgba.len() {
            let msg = format!(
                "Tried to create a texture of size {}x{}, but gave {} bytes of data (expected {})",
                width,
                height,
                rgba.len(),
                expected_bytes
            );
            return Err(GameError::ResourceLoadError(msg));
        }

        let kind = gfx::texture::Kind::D2(width, height, gfx::texture::AaMode::Single);
        use gfx::memory::Bind;
        let gfx::format::Format(surface_format, channel_type) = color_format;
        let texinfo = gfx::texture::Info {
            kind,
            levels: 1,
            format: surface_format,
            bind: Bind::SHADER_RESOURCE
                | Bind::RENDER_TARGET
                | Bind::TRANSFER_SRC
                | Bind::TRANSFER_DST,
            usage: gfx::memory::Usage::Dynamic,
        };
        let raw_tex = factory.create_texture_raw(
            texinfo,
            Some(channel_type),
            Some((&[&rgba], gfx::texture::Mipmap::Provided)),
        )?;
        let resource_desc = gfx::texture::ResourceDesc {
            channel: channel_type,
            layer: None,
            min: 0,
            max: raw_tex.get_info().levels - 1,
            swizzle: gfx::format::Swizzle::new(),
        };
        let raw_view = factory.view_texture_as_shader_resource_raw(&raw_tex, resource_desc)?;
        Ok(Self {
            texture: raw_view,
            texture_handle: raw_tex,
            sampler_info: *sampler_info,
            blend_mode: None,
            width,
            height,
            debug_id,
        })
    }

    /// A helper function to get the raw gfx texture handle
    pub fn get_raw_texture_handle(&self) -> gfx::handle::RawTexture<B::Resources> {
        self.texture_handle.clone()
    }

    /// A helper function to get the raw gfx texture view
    pub fn get_raw_texture_view(&self) -> gfx::handle::RawShaderResourceView<B::Resources> {
        self.texture.clone()
    }
}

/// In-GPU-memory image data available to be drawn on the screen,
/// using the OpenGL backend.
///
/// Under the hood this is just an `Arc`'ed texture handle and
/// some metadata, so cloning it is fairly cheap; it doesn't
/// make another copy of the underlying image data.
pub type Image = ImageGeneric<GlBackendSpec>;

/// The supported formats for saving an image.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum ImageFormat {
    /// .png image format (defaults to RGBA with 8-bit channels.)
    Png,
}

impl Image {
    /// Load a new image from the file at the given path. The documentation for the
    /// [`filesystem`](../filesystem/index.html) module explains how the path must be specified.
    pub fn new<P: AsRef<path::Path>>(context: &mut Context, path: P) -> GameResult<Self> {
        let mut buf = Vec::new();
        let mut reader = context.filesystem.open(path)?;
        let _ = reader.read_to_end(&mut buf)?;
        Self::from_bytes(context, &buf)
    }

    /// Creates a new `Image` from the given buffer, which should contain an image encoded
    /// in a supported image file format.
    pub fn from_bytes(context: &mut Context, bytes: &[u8]) -> GameResult<Self> {
        let img = image::load_from_memory(bytes)?.to_rgba8();
        let (width, height) = img.dimensions();
        let better_width = u16::try_from(width)
            .map_err(|_| GameError::ResourceLoadError(String::from("Image width > u16::MAX")))?;
        let better_height = u16::try_from(height)
            .map_err(|_| GameError::ResourceLoadError(String::from("Image height > u16::MAX")))?;
        Self::from_rgba8(context, better_width, better_height, &img)
    }

    /// Creates a new `Image` from the given buffer of `u8` RGBA values.
    ///
    /// The pixel layout is row-major.  That is,
    /// the first 4 `u8` values make the top-left pixel in the `Image`, the
    /// next 4 make the next pixel in the same row, and so on to the end of
    /// the row.  The next `width * 4` values make up the second row, and so
    /// on.
    pub fn from_rgba8(
        context: &mut Context,
        width: u16,
        height: u16,
        rgba: &[u8],
    ) -> GameResult<Self> {
        let debug_id = DebugId::get(context);
        let color_format = context.gfx_context.color_format();
        Self::make_raw(
            &mut *context.gfx_context.factory,
            &context.gfx_context.default_sampler_info,
            width,
            height,
            rgba,
            color_format,
            debug_id,
        )
    }

    /// Dumps the `Image`'s data to a `Vec` of `u8` RGBA values.
    pub fn to_rgba8(&self, ctx: &mut Context) -> GameResult<Vec<u8>> {
        use gfx::memory::Typed;
        use gfx::traits::FactoryExt;

        let gfx = &mut ctx.gfx_context;
        let w = self.width;
        let h = self.height;

        let format = gfx.color_format();

        let dl_buffer = &mut gfx.to_rgba8_buffer;
        // check if it's big enough and recreate it if not
        let size_needed = usize::from(w) * usize::from(h) * 4;
        if dl_buffer.len() != size_needed {
            *dl_buffer = gfx.factory.create_download_buffer::<u8>(size_needed)?;
        }

        let encoder = &mut gfx.encoder;

        encoder.copy_texture_to_buffer_raw(
            &self.texture_handle,
            None,
            gfx::texture::RawImageInfo {
                xoffset: 0,
                yoffset: 0,
                zoffset: 0,
                width: w,
                height: h,
                depth: 0,
                format,
                mipmap: 0,
            },
            dl_buffer.raw(),
            0,
        )?;
        encoder.flush(&mut *gfx.device);

        let reader = gfx.factory.read_mapping(dl_buffer)?.to_vec();
        Ok(reader)
    }

    /// Encode the `Image` to the given file format and
    /// write it out to the given path.
    ///
    /// See the [`filesystem`](../filesystem/index.html) module docs for where exactly
    /// the file will end up.
    pub fn encode<P: AsRef<path::Path>>(
        &self,
        ctx: &mut Context,
        format: ImageFormat,
        path: P,
    ) -> GameResult {
        use std::io;
        let data = self.to_rgba8(ctx)?;
        let f = filesystem::create(ctx, path)?;
        let writer = &mut io::BufWriter::new(f);
        let color_format = image::ColorType::Rgba8;
        match format {
            ImageFormat::Png => image::png::PngEncoder::new(writer)
                .encode(
                    &data,
                    u32::from(self.width),
                    u32::from(self.height),
                    color_format,
                )
                .map_err(Into::into),
        }
    }

    /// A little helper function that creates a new `Image` that is just
    /// a solid square of the given size and color.  Mainly useful for
    /// debugging.
    pub fn solid(context: &mut Context, size: u16, color: Color) -> GameResult<Self> {
        let (r, g, b, a) = color.into();
        let pixel_array: [u8; 4] = [r, g, b, a];
        let size_squared = usize::from(size) * usize::from(size);
        let mut buffer = Vec::with_capacity(size_squared);
        for _i in 0..size_squared {
            buffer.extend(&pixel_array[..]);
        }
        Image::from_rgba8(context, size, size, &buffer)
    }

    /// Return the width of the image.
    pub fn width(&self) -> u16 {
        self.width
    }

    /// Return the height of the image.
    pub fn height(&self) -> u16 {
        self.height
    }

    /// Get the filter mode for the image.
    pub fn filter(&self) -> FilterMode {
        self.sampler_info.filter.into()
    }

    /// Set the filter mode for the image.
    pub fn set_filter(&mut self, mode: FilterMode) {
        self.sampler_info.filter = mode.into();
    }

    /// Returns the dimensions of the image.
    pub fn dimensions(&self) -> Rect {
        Rect::new(0.0, 0.0, f32::from(self.width()), f32::from(self.height()))
    }

    /// Gets the `Image`'s `WrapMode` along the X and Y axes.
    pub fn wrap(&self) -> (WrapMode, WrapMode) {
        (self.sampler_info.wrap_mode.0, self.sampler_info.wrap_mode.1)
    }

    /// Sets the `Image`'s `WrapMode` along the X and Y axes.
    pub fn set_wrap(&mut self, wrap_x: WrapMode, wrap_y: WrapMode) {
        self.sampler_info.wrap_mode.0 = wrap_x;
        self.sampler_info.wrap_mode.1 = wrap_y;
    }
}

impl fmt::Debug for Image {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "<Image: {}x{}, {:p}, texture address {:p}, sampler: {:?}>",
            self.width(),
            self.height(),
            self,
            &self.texture,
            &self.sampler_info
        )
    }
}

impl Drawable for Image {
    fn draw(&self, ctx: &mut Context, param: DrawParam) -> GameResult {
        self.debug_id.assert(ctx);

        let src_width = param.src.w;
        let src_height = param.src.h;
        // We have to mess with the scale to make everything
        // be its-unit-size-in-pixels.
        let scale_x = src_width * f32::from(self.width);
        let scale_y = src_height * f32::from(self.height);
        let new_param = match param.trans {
            Transform::Values { scale, .. } => param.scale(mint::Vector2 {
                x: scale.x * scale_x,
                y: scale.y * scale_y,
            }),
            Transform::Matrix(m) => param.transform(
                Matrix4::from(m) * Matrix4::from_scale(glam::vec3(scale_x, scale_y, 1.0)),
            ),
        };

        draw_image_raw(self, ctx, new_param)
    }

    fn dimensions(&self, _: &mut Context) -> Option<graphics::Rect> {
        Some(self.dimensions())
    }

    fn set_blend_mode(&mut self, mode: Option<BlendMode>) {
        self.blend_mode = mode;
    }

    fn blend_mode(&self) -> Option<BlendMode> {
        self.blend_mode
    }
}

pub(crate) fn draw_image_raw(image: &Image, ctx: &mut Context, param: DrawParam) -> GameResult {
    let gfx = &mut ctx.gfx_context;

    gfx.update_instance_properties(param)?;
    let sampler = gfx
        .samplers
        .get_or_insert(image.sampler_info, gfx.factory.as_mut());
    gfx.data.vbuf = gfx.quad_vertex_buffer.clone();
    let typed_thingy = gfx
        .backend_spec
        .raw_to_typed_shader_resource(image.texture.clone());
    gfx.data.tex = (typed_thingy, sampler);
    let previous_mode: Option<BlendMode> = if let Some(mode) = image.blend_mode {
        let current_mode = gfx.blend_mode();
        if current_mode != mode {
            gfx.set_blend_mode(mode)?;
            Some(current_mode)
        } else {
            None
        }
    } else {
        None
    };

    gfx.draw(None)?;
    if let Some(mode) = previous_mode {
        gfx.set_blend_mode(mode)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ContextBuilder;
    #[test]
    fn test_invalid_image_size() {
        let (ctx, _) = &mut ContextBuilder::new("unittest", "unittest").build().unwrap();
        let _i = assert!(Image::from_rgba8(ctx, 0, 0, &[]).is_err());
        let _i = assert!(Image::from_rgba8(ctx, 3432, 432, &[]).is_err());
        let _i = Image::from_rgba8(ctx, 2, 2, &[99; 16]).unwrap();
    }
}
