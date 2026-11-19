//! NDK camera: the back camera streams YUV frames into an image reader, and the luma plane of the
//! newest frame is handed to the QR decoder. Nothing is shown or stored; a frame lives only for
//! the decode.

use std::ffi::{CStr, c_void};
use std::ptr;

use ndk_sys as ffi;

use crate::scan::decode_luma;

/// Small enough to decode within a frame budget, large enough for a terminal QR code.
const WIDTH: i32 = 1280;
const HEIGHT: i32 = 720;

pub struct Camera {
    manager: *mut ffi::ACameraManager,
    device: *mut ffi::ACameraDevice,
    reader: *mut ffi::AImageReader,
    session: *mut ffi::ACameraCaptureSession,
    request: *mut ffi::ACaptureRequest,
    target: *mut ffi::ACameraOutputTarget,
    output: *mut ffi::ACaptureSessionOutput,
    container: *mut ffi::ACaptureSessionOutputContainer,
}

unsafe extern "C" fn on_disconnected(_: *mut c_void, _: *mut ffi::ACameraDevice) {}
unsafe extern "C" fn on_error(_: *mut c_void, _: *mut ffi::ACameraDevice, _: i32) {}
unsafe extern "C" fn on_session(_: *mut c_void, _: *mut ffi::ACameraCaptureSession) {}

fn ok(status: ffi::camera_status_t) -> Result<(), String> {
    if status == ffi::camera_status_t::ACAMERA_OK {
        Ok(())
    } else {
        Err(format!("camera error {}", status.0))
    }
}

impl Camera {
    /// Opens the back camera and starts streaming, or says why it could not.
    pub fn start() -> Result<Camera, String> {
        let mut camera = Camera {
            manager: ptr::null_mut(),
            device: ptr::null_mut(),
            reader: ptr::null_mut(),
            session: ptr::null_mut(),
            request: ptr::null_mut(),
            target: ptr::null_mut(),
            output: ptr::null_mut(),
            container: ptr::null_mut(),
        };
        // SAFETY: each NDK object is created once, stored in `camera` the moment it exists, and
        // released by `Drop` in reverse order, so an early return frees what was made so far.
        unsafe {
            camera.manager = ffi::ACameraManager_create();
            let id = camera.back_camera_id()?;
            let mut callbacks = ffi::ACameraDevice_StateCallbacks {
                context: ptr::null_mut(),
                onDisconnected: Some(on_disconnected),
                onError: Some(on_error),
            };
            ok(ffi::ACameraManager_openCamera(
                camera.manager,
                id.as_ptr(),
                &mut callbacks,
                &mut camera.device,
            ))?;
            if ffi::AImageReader_new(
                WIDTH,
                HEIGHT,
                ffi::AIMAGE_FORMATS::AIMAGE_FORMAT_YUV_420_888.0 as i32,
                2,
                &mut camera.reader,
            ) != ffi::media_status_t::AMEDIA_OK
            {
                return Err("the image reader could not start".to_string());
            }
            let mut window = ptr::null_mut();
            if ffi::AImageReader_getWindow(camera.reader, &mut window)
                != ffi::media_status_t::AMEDIA_OK
            {
                return Err("the image reader has no surface".to_string());
            }
            ok(ffi::ACaptureSessionOutputContainer_create(
                &mut camera.container,
            ))?;
            ok(ffi::ACaptureSessionOutput_create(
                window,
                &mut camera.output,
            ))?;
            ok(ffi::ACaptureSessionOutputContainer_add(
                camera.container,
                camera.output,
            ))?;
            ok(ffi::ACameraOutputTarget_create(window, &mut camera.target))?;
            ok(ffi::ACameraDevice_createCaptureRequest(
                camera.device,
                ffi::ACameraDevice_request_template::TEMPLATE_PREVIEW,
                &mut camera.request,
            ))?;
            ok(ffi::ACaptureRequest_addTarget(
                camera.request,
                camera.target,
            ))?;
            let session_callbacks = ffi::ACameraCaptureSession_stateCallbacks {
                context: ptr::null_mut(),
                onClosed: Some(on_session),
                onReady: Some(on_session),
                onActive: Some(on_session),
            };
            ok(ffi::ACameraDevice_createCaptureSession(
                camera.device,
                camera.container,
                &session_callbacks,
                &mut camera.session,
            ))?;
            ok(ffi::ACameraCaptureSession_setRepeatingRequest(
                camera.session,
                ptr::null_mut(),
                1,
                &mut camera.request,
                ptr::null_mut(),
            ))?;
        }
        Ok(camera)
    }

    /// The id of the first back-facing camera, else the first camera there is.
    ///
    /// # Safety
    /// `self.manager` must be a live camera manager.
    unsafe fn back_camera_id(&self) -> Result<std::ffi::CString, String> {
        // SAFETY: the caller guarantees the manager; the id list is freed before returning and
        // the chosen id is copied out first.
        unsafe {
            let mut list = ptr::null_mut();
            ok(ffi::ACameraManager_getCameraIdList(self.manager, &mut list))?;
            let count = (*list).numCameras;
            let mut chosen: Option<std::ffi::CString> = None;
            for i in 0..count {
                let id = *(*list).cameraIds.add(i as usize);
                let mut metadata = ptr::null_mut();
                if ffi::ACameraManager_getCameraCharacteristics(self.manager, id, &mut metadata)
                    != ffi::camera_status_t::ACAMERA_OK
                {
                    continue;
                }
                let mut entry = std::mem::zeroed::<ffi::ACameraMetadata_const_entry>();
                let facing = ffi::ACameraMetadata_getConstEntry(
                    metadata,
                    ffi::acamera_metadata_tag::ACAMERA_LENS_FACING.0,
                    &mut entry,
                ) == ffi::camera_status_t::ACAMERA_OK
                    && entry.count > 0
                    && u32::from(*entry.data.u8_)
                        == ffi::acamera_metadata_enum_acamera_lens_facing::ACAMERA_LENS_FACING_BACK
                            .0;
                ffi::ACameraMetadata_free(metadata);
                let owned = CStr::from_ptr(id).to_owned();
                if facing {
                    chosen = Some(owned);
                    break;
                }
                chosen.get_or_insert(owned);
            }
            ffi::ACameraManager_deleteCameraIdList(list);
            chosen.ok_or_else(|| "this phone reports no camera".to_string())
        }
    }

    /// Decodes the newest frame, if one is ready and holds a QR code.
    pub fn poll(&mut self) -> Option<String> {
        // SAFETY: `reader` is live while `self` is; the image is deleted before returning, and the
        // luma plane is copied row by row so the decoder never sees the driver's stride padding.
        unsafe {
            let mut image = ptr::null_mut();
            if ffi::AImageReader_acquireLatestImage(self.reader, &mut image)
                != ffi::media_status_t::AMEDIA_OK
                || image.is_null()
            {
                return None;
            }
            let (mut width, mut height, mut stride, mut len) = (0, 0, 0, 0);
            let mut data: *mut u8 = ptr::null_mut();
            ffi::AImage_getWidth(image, &mut width);
            ffi::AImage_getHeight(image, &mut height);
            ffi::AImage_getPlaneRowStride(image, 0, &mut stride);
            let got = ffi::AImage_getPlaneData(image, 0, &mut data, &mut len)
                == ffi::media_status_t::AMEDIA_OK;
            let decoded = if got && !data.is_null() && width > 0 && height > 0 && stride >= width {
                let plane = std::slice::from_raw_parts(data, len as usize);
                let (w, h, s) = (width as usize, height as usize, stride as usize);
                let mut luma = Vec::with_capacity(w * h);
                for row in 0..h {
                    match plane.get(row * s..row * s + w) {
                        Some(line) => luma.extend_from_slice(line),
                        None => break,
                    }
                }
                (luma.len() == w * h)
                    .then(|| decode_luma(w, h, &luma))
                    .flatten()
            } else {
                None
            };
            ffi::AImage_delete(image);
            decoded
        }
    }
}

impl Drop for Camera {
    fn drop(&mut self) {
        // SAFETY: every pointer is either null or a live object made in `start`; each is freed
        // once, the session and request before the device, the device before the reader.
        unsafe {
            if !self.session.is_null() {
                ffi::ACameraCaptureSession_stopRepeating(self.session);
                ffi::ACameraCaptureSession_close(self.session);
            }
            if !self.request.is_null() {
                ffi::ACaptureRequest_free(self.request);
            }
            if !self.target.is_null() {
                ffi::ACameraOutputTarget_free(self.target);
            }
            if !self.output.is_null() {
                ffi::ACaptureSessionOutput_free(self.output);
            }
            if !self.container.is_null() {
                ffi::ACaptureSessionOutputContainer_free(self.container);
            }
            if !self.device.is_null() {
                ffi::ACameraDevice_close(self.device);
            }
            if !self.reader.is_null() {
                ffi::AImageReader_delete(self.reader);
            }
            if !self.manager.is_null() {
                ffi::ACameraManager_delete(self.manager);
            }
        }
    }
}
