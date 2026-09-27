use ash::vk;
use std::mem;

impl super::Surface {
    pub fn info(&self) -> crate::SurfaceInfo {
        crate::SurfaceInfo {
            format: self.swapchain.format,
            alpha: self.swapchain.alpha,
        }
    }

    unsafe fn deinit_swapchain(&mut self, raw_device: &ash::Device) {
        let _ = raw_device.device_wait_idle();
        self.device
            .destroy_swapchain(mem::take(&mut self.swapchain.raw), None);
        for frame in self.frames.drain(..) {
            raw_device.destroy_image_view(frame.view, None);
            raw_device.destroy_semaphore(frame.acquire_semaphore, None);
            raw_device.destroy_semaphore(frame.present_semaphore, None);
        }
    }

    /// Whether the window system took this surface away. A lost surface
    /// hands out frames without an image until `Context::recreate_surface`
    /// replaces it.
    pub fn is_lost(&self) -> bool {
        self.lost
    }

    // A frame that is not presented, for when no image can be acquired.
    fn frame_without_image(&self) -> super::Frame {
        super::Frame {
            internal: self.frames.first().copied().unwrap_or_default(),
            swapchain: self.swapchain,
            image_index: None,
        }
    }

    pub fn acquire_frame(&mut self) -> super::Frame {
        if self.lost || self.swapchain.raw == vk::SwapchainKHR::null() {
            return self.frame_without_image();
        }
        if fake_surface_lost() {
            // stderr, not `log`: a check has to see it whether or not the
            // application installed a logger.
            eprintln!("blade: faking a lost surface ({FAKE_SURFACE_LOST})");
            self.lost = true;
            return self.frame_without_image();
        }
        let acquire_semaphore = self.next_semaphore;
        match unsafe {
            self.device.acquire_next_image(
                self.swapchain.raw,
                !0,
                acquire_semaphore,
                vk::Fence::null(),
            )
        } {
            Ok((index, _suboptimal)) => {
                self.next_semaphore = mem::replace(
                    &mut self.frames[index as usize].acquire_semaphore,
                    acquire_semaphore,
                );
                super::Frame {
                    internal: self.frames[index as usize],
                    swapchain: self.swapchain,
                    image_index: Some(index),
                }
            }
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                log::warn!("Acquire failed because the surface is out of date");
                self.frame_without_image()
            }
            // 0.7.1 panicked here, and took the whole application down with a
            // window that a driver reset or a display change had merely
            // invalidated.
            Err(vk::Result::ERROR_SURFACE_LOST_KHR) => {
                log::warn!("Acquire failed because the surface is lost");
                self.lost = true;
                self.frame_without_image()
            }
            Err(other) => {
                log::error!("Acquire image error: {}", other);
                self.frame_without_image()
            }
        }
    }
}

/// Makes every Nth acquire report a lost surface, N from this variable - the
/// only way to see a recovery on demand: a real `VK_ERROR_SURFACE_LOST_KHR`
/// comes from a driver reset or a display going away.
const FAKE_SURFACE_LOST: &str = "BLADE_FAKE_SURFACE_LOST";

fn fake_surface_lost() -> bool {
    use std::sync::{
        atomic::{AtomicU64, Ordering},
        OnceLock,
    };
    static EVERY: OnceLock<Option<u64>> = OnceLock::new();
    static ACQUIRES: AtomicU64 = AtomicU64::new(0);
    let every = *EVERY.get_or_init(|| {
        std::env::var(FAKE_SURFACE_LOST)
            .ok()
            .and_then(|value| value.trim().parse::<u64>().ok())
            .filter(|&n| n > 0)
    });
    match every {
        Some(n) => (ACQUIRES.fetch_add(1, Ordering::Relaxed) + 1) % n == 0,
        None => false,
    }
}

impl super::Context {
    pub fn create_surface<
        I: raw_window_handle::HasWindowHandle + raw_window_handle::HasDisplayHandle,
    >(
        &self,
        window: &I,
    ) -> Result<super::Surface, crate::NotSupportedError> {
        let khr_swapchain = self
            .device
            .swapchain
            .clone()
            .ok_or(crate::NotSupportedError::NoSupportedDeviceFound)?;

        let raw = unsafe {
            ash_window::create_surface(
                &self.entry,
                &self.instance.core,
                window.display_handle().unwrap().as_raw(),
                window.window_handle().unwrap().as_raw(),
                None,
            )
            .map_err(super::PlatformError::Init)?
        };

        let khr_surface = self
            .instance
            .surface
            .as_ref()
            .ok_or(crate::NotSupportedError::PlatformNotSupported)?;
        if unsafe {
            khr_surface.get_physical_device_surface_support(
                self.physical_device,
                self.queue_family_index,
                raw,
            ) != Ok(true)
        } {
            log::warn!("Rejected for not presenting to the window surface");
            // 0.7.1 leaked the surface here, once per retry of a lost one.
            unsafe { khr_surface.destroy_surface(raw, None) };
            return Err(crate::NotSupportedError::PlatformNotSupported);
        }

        let mut surface_info = vk::PhysicalDeviceSurfaceInfo2KHR {
            surface: raw,
            ..Default::default()
        };
        let mut fullscreen_exclusive_win32 = vk::SurfaceFullScreenExclusiveWin32InfoEXT::default();
        surface_info = surface_info.push_next(&mut fullscreen_exclusive_win32);
        let mut fullscreen_exclusive_ext = vk::SurfaceCapabilitiesFullScreenExclusiveEXT::default();
        let mut capabilities2_khr =
            vk::SurfaceCapabilities2KHR::default().push_next(&mut fullscreen_exclusive_ext);
        let _ = unsafe {
            self.instance
                .get_surface_capabilities2
                .as_ref()
                .unwrap()
                .get_physical_device_surface_capabilities2(
                    self.physical_device,
                    &surface_info,
                    &mut capabilities2_khr,
                )
        };
        log::debug!("{:?}", capabilities2_khr.surface_capabilities);

        let semaphore_create_info = vk::SemaphoreCreateInfo::default();
        let next_semaphore = unsafe {
            self.device
                .core
                .create_semaphore(&semaphore_create_info, None)
                .unwrap()
        };

        Ok(super::Surface {
            device: khr_swapchain,
            raw,
            frames: Vec::new(),
            next_semaphore,
            swapchain: super::Swapchain {
                raw: vk::SwapchainKHR::null(),
                format: crate::TextureFormat::Rgba8Unorm,
                alpha: crate::AlphaMode::Ignored,
                target_size: [0; 2],
            },
            full_screen_exclusive: fullscreen_exclusive_ext.full_screen_exclusive_supported != 0,
            lost: false,
        })
    }

    /// Gives a lost surface a new `VkSurfaceKHR` for the same window and
    /// configures it. On failure the surface stays as it was, lost, and the
    /// call can be repeated.
    pub fn recreate_surface<
        I: raw_window_handle::HasWindowHandle + raw_window_handle::HasDisplayHandle,
    >(
        &self,
        surface: &mut super::Surface,
        window: &I,
        config: crate::SurfaceConfig,
    ) -> Result<(), crate::NotSupportedError> {
        let fresh = self.create_surface(window)?;
        // The old swapchain goes before the new one is made: a window has
        // one swapchain at a time (`VK_ERROR_NATIVE_WINDOW_IN_USE_KHR`).
        let mut old = mem::replace(surface, fresh);
        self.destroy_surface(&mut old);
        self.reconfigure_surface(surface, config);
        if surface.lost {
            return Err(super::PlatformError::Init(vk::Result::ERROR_SURFACE_LOST_KHR).into());
        }
        Ok(())
    }

    pub fn destroy_surface(&self, surface: &mut super::Surface) {
        unsafe {
            surface.deinit_swapchain(&self.device.core);
            self.device
                .core
                .destroy_semaphore(surface.next_semaphore, None)
        };
        if let Some(ref surface_instance) = self.instance.surface {
            unsafe { surface_instance.destroy_surface(surface.raw, None) };
        }
    }

    /// Leaves a lost surface as it is: see `Surface::is_lost`.
    pub fn reconfigure_surface(&self, surface: &mut super::Surface, config: crate::SurfaceConfig) {
        if surface.lost {
            log::warn!("Not reconfiguring a lost surface");
            return;
        }
        let khr_surface = self.instance.surface.as_ref().unwrap();

        let result = unsafe {
            khr_surface.get_physical_device_surface_capabilities(self.physical_device, surface.raw)
        };
        let Some(capabilities) = unless_lost(surface, "Surface capabilities", result) else {
            return;
        };
        if config.size.width < capabilities.min_image_extent.width
            || config.size.width > capabilities.max_image_extent.width
            || config.size.height < capabilities.min_image_extent.height
            || config.size.height > capabilities.max_image_extent.height
        {
            log::warn!(
                "Requested size {}x{} is outside of surface capabilities",
                config.size.width,
                config.size.height
            );
        }

        let (alpha, composite_alpha) = if config.transparent {
            if capabilities
                .supported_composite_alpha
                .contains(vk::CompositeAlphaFlagsKHR::POST_MULTIPLIED)
            {
                (
                    crate::AlphaMode::PostMultiplied,
                    vk::CompositeAlphaFlagsKHR::POST_MULTIPLIED,
                )
            } else if capabilities
                .supported_composite_alpha
                .contains(vk::CompositeAlphaFlagsKHR::PRE_MULTIPLIED)
            {
                (
                    crate::AlphaMode::PreMultiplied,
                    vk::CompositeAlphaFlagsKHR::PRE_MULTIPLIED,
                )
            } else {
                log::error!(
                    "No composite alpha flag for transparency: {:?}",
                    capabilities.supported_composite_alpha
                );
                (
                    crate::AlphaMode::Ignored,
                    vk::CompositeAlphaFlagsKHR::OPAQUE,
                )
            }
        } else {
            (
                crate::AlphaMode::Ignored,
                vk::CompositeAlphaFlagsKHR::OPAQUE,
            )
        };

        let (requested_frame_count, mode_preferences) = match config.display_sync {
            crate::DisplaySync::Block => (3, [vk::PresentModeKHR::FIFO].as_slice()),
            crate::DisplaySync::Recent => (
                3,
                [
                    vk::PresentModeKHR::MAILBOX,
                    vk::PresentModeKHR::FIFO_RELAXED,
                    vk::PresentModeKHR::IMMEDIATE,
                ]
                .as_slice(),
            ),
            crate::DisplaySync::Tear => (2, [vk::PresentModeKHR::IMMEDIATE].as_slice()),
        };
        let effective_frame_count = requested_frame_count.max(capabilities.min_image_count);

        let result = unsafe {
            khr_surface.get_physical_device_surface_present_modes(self.physical_device, surface.raw)
        };
        let Some(present_modes) = unless_lost(surface, "Surface present modes", result) else {
            return;
        };
        let present_mode = *mode_preferences
            .iter()
            .find(|mode| present_modes.contains(mode))
            .unwrap();
        log::info!("Using surface present mode {:?}", present_mode);

        let queue_families = [self.queue_family_index];

        let mut supported_formats = Vec::new();
        let (format, surface_format) = if surface.swapchain.target_size[0] > 0 {
            let format = surface.swapchain.format;
            log::info!("Retaining current format: {:?}", format);
            let vk_color_space = match (format, config.color_space) {
                (crate::TextureFormat::Bgra8Unorm, crate::ColorSpace::Srgb) => {
                    vk::ColorSpaceKHR::SRGB_NONLINEAR
                }
                (crate::TextureFormat::Bgra8Unorm, crate::ColorSpace::Linear) => {
                    vk::ColorSpaceKHR::EXTENDED_SRGB_LINEAR_EXT
                }
                (crate::TextureFormat::Bgra8UnormSrgb, crate::ColorSpace::Linear) => {
                    vk::ColorSpaceKHR::default()
                }
                _ => panic!(
                    "Unexpected format {:?} under color space {:?}",
                    format, config.color_space
                ),
            };
            (
                format,
                vk::SurfaceFormatKHR {
                    format: super::map_texture_format(format),
                    color_space: vk_color_space,
                },
            )
        } else {
            let result = unsafe {
                khr_surface.get_physical_device_surface_formats(self.physical_device, surface.raw)
            };
            let Some(formats) = unless_lost(surface, "Surface formats", result) else {
                return;
            };
            supported_formats = formats;
            match config.color_space {
                crate::ColorSpace::Linear => {
                    let surface_format = vk::SurfaceFormatKHR {
                        format: vk::Format::B8G8R8A8_UNORM,
                        color_space: vk::ColorSpaceKHR::EXTENDED_SRGB_LINEAR_EXT,
                    };
                    if supported_formats.contains(&surface_format) {
                        log::info!("Using linear SRGB color space");
                        (crate::TextureFormat::Bgra8Unorm, surface_format)
                    } else {
                        (
                            crate::TextureFormat::Bgra8UnormSrgb,
                            vk::SurfaceFormatKHR {
                                format: vk::Format::B8G8R8A8_SRGB,
                                color_space: vk::ColorSpaceKHR::default(),
                            },
                        )
                    }
                }
                crate::ColorSpace::Srgb => (
                    crate::TextureFormat::Bgra8Unorm,
                    vk::SurfaceFormatKHR {
                        format: vk::Format::B8G8R8A8_UNORM,
                        color_space: vk::ColorSpaceKHR::SRGB_NONLINEAR,
                    },
                ),
            }
        };
        if !supported_formats.is_empty() && !supported_formats.contains(&surface_format) {
            log::error!("Surface formats are incompatible: {:?}", supported_formats);
        }

        let vk_usage = super::resource::map_texture_usage(config.usage, crate::TexelAspects::COLOR);
        if !capabilities.supported_usage_flags.contains(vk_usage) {
            log::error!(
                "Surface usages are incompatible: {:?}",
                capabilities.supported_usage_flags
            );
        }

        let mut full_screen_exclusive_info = vk::SurfaceFullScreenExclusiveInfoEXT {
            full_screen_exclusive: if config.allow_exclusive_full_screen {
                vk::FullScreenExclusiveEXT::ALLOWED
            } else {
                vk::FullScreenExclusiveEXT::DISALLOWED
            },
            ..Default::default()
        };

        let mut create_info = vk::SwapchainCreateInfoKHR {
            surface: surface.raw,
            min_image_count: effective_frame_count,
            image_format: surface_format.format,
            image_color_space: surface_format.color_space,
            image_extent: vk::Extent2D {
                width: config.size.width,
                height: config.size.height,
            },
            image_array_layers: 1,
            image_usage: vk_usage,
            pre_transform: vk::SurfaceTransformFlagsKHR::IDENTITY,
            composite_alpha,
            present_mode,
            old_swapchain: surface.swapchain.raw,
            ..Default::default()
        }
        .queue_family_indices(&queue_families);

        if surface.full_screen_exclusive {
            assert!(self.device.full_screen_exclusive.is_some());
            create_info = create_info.push_next(&mut full_screen_exclusive_info);
            log::info!(
                "Configuring exclusive full screen: {}",
                config.allow_exclusive_full_screen
            );
        }
        let result = unsafe { surface.device.create_swapchain(&create_info, None) };
        let Some(raw_swapchain) = unless_lost(surface, "Swapchain creation", result) else {
            return;
        };

        unsafe {
            surface.deinit_swapchain(&self.device.core);
        }

        let images = unsafe { surface.device.get_swapchain_images(raw_swapchain).unwrap() };
        let target_size = [config.size.width as u16, config.size.height as u16];
        let subresource_range = vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: 0,
            level_count: 1,
            base_array_layer: 0,
            layer_count: 1,
        };
        for image in images {
            let view_create_info = vk::ImageViewCreateInfo {
                image,
                view_type: vk::ImageViewType::TYPE_2D,
                format: surface_format.format,
                subresource_range,
                ..Default::default()
            };
            let view = unsafe {
                self.device
                    .core
                    .create_image_view(&view_create_info, None)
                    .unwrap()
            };
            let semaphore_create_info = vk::SemaphoreCreateInfo::default();
            let acquire_semaphore = unsafe {
                self.device
                    .core
                    .create_semaphore(&semaphore_create_info, None)
                    .unwrap()
            };
            let present_semaphore = unsafe {
                self.device
                    .core
                    .create_semaphore(&semaphore_create_info, None)
                    .unwrap()
            };
            surface.frames.push(super::InternalFrame {
                acquire_semaphore,
                present_semaphore,
                image,
                view,
            });
        }
        surface.swapchain = super::Swapchain {
            raw: raw_swapchain,
            format,
            alpha,
            target_size,
        };
    }
}

// A surface call's value, or `None` once the call finds the surface lost and
// marks it so. Any other error panics, as the `unwrap` it replaces did.
fn unless_lost<T>(
    surface: &mut super::Surface,
    what: &str,
    result: Result<T, vk::Result>,
) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(vk::Result::ERROR_SURFACE_LOST_KHR) => {
            log::warn!("{what} failed because the surface is lost");
            surface.lost = true;
            None
        }
        Err(other) => panic!("{what} failed: {other}"),
    }
}
