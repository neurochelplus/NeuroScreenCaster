use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::Duration;

pub struct LoopbackCaptureHandle {
    pub stop_flag: Arc<AtomicBool>,
    pub join_handle: JoinHandle<Result<(), String>>,
}

pub fn start_system_loopback_capture(
    output_path: PathBuf,
) -> Result<LoopbackCaptureHandle, String> {
    let stop_flag = Arc::new(AtomicBool::new(false));
    let stop_for_thread = Arc::clone(&stop_flag);
    let (ready_tx, ready_rx) = mpsc::sync_channel::<Result<(), String>>(1);

    let join_handle = std::thread::Builder::new()
        .name("wasapi-loopback-capture".to_string())
        .spawn(move || run_loopback_capture_thread(output_path, stop_for_thread, ready_tx))
        .map_err(|e| format!("Failed to spawn WASAPI loopback capture thread: {e}"))?;

    let mut join_handle = Some(join_handle);
    let readiness = ready_rx.recv_timeout(Duration::from_secs(2));
    match readiness {
        Ok(Ok(())) => Ok(LoopbackCaptureHandle {
            stop_flag,
            join_handle: join_handle
                .take()
                .expect("loopback capture thread handle must exist"),
        }),
        Ok(Err(err)) => {
            stop_flag.store(true, Ordering::Relaxed);
            if let Some(handle) = join_handle.take() {
                let _ = handle.join();
            }
            Err(err)
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            stop_flag.store(true, Ordering::Relaxed);
            if let Some(handle) = join_handle.take() {
                let _ = handle.join();
            }
            Err("Timed out while starting WASAPI loopback audio capture".to_string())
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            stop_flag.store(true, Ordering::Relaxed);
            if let Some(handle) = join_handle.take() {
                match handle.join() {
                    Ok(Err(err)) => return Err(err),
                    Ok(Ok(())) => {}
                    Err(_) => {
                        return Err(
                            "WASAPI loopback capture thread panicked during startup".to_string()
                        );
                    }
                }
            }
            Err("WASAPI loopback capture thread exited unexpectedly during startup".to_string())
        }
    }
}

#[cfg(target_os = "windows")]
fn run_loopback_capture_thread(
    output_path: PathBuf,
    stop_flag: Arc<AtomicBool>,
    ready_tx: mpsc::SyncSender<Result<(), String>>,
) -> Result<(), String> {
    use std::ptr;

    use windows::Win32::Media::Audio::{
        eConsole, eRender, IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator,
        MMDeviceEnumerator, AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED,
        AUDCLNT_STREAMFLAGS_LOOPBACK, WAVEFORMATEX,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
        COINIT_MULTITHREADED,
    };

    struct ComApartment;
    impl ComApartment {
        fn initialize() -> Result<Self, String> {
            unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
                .map_err(|e| format!("WASAPI loopback COM init failed: {e}"))?;
            Ok(Self)
        }
    }
    impl Drop for ComApartment {
        fn drop(&mut self) {
            unsafe { CoUninitialize() };
        }
    }

    let run = || -> Result<(), String> {
        let _com = ComApartment::initialize()?;

        let enumerator: IMMDeviceEnumerator =
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
                .map_err(|e| format!("WASAPI loopback failed to create device enumerator: {e}"))?;

        let render_device = unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole) }
            .map_err(|e| format!("WASAPI loopback failed to open default render endpoint: {e}"))?;

        let audio_client: IAudioClient = unsafe { render_device.Activate(CLSCTX_ALL, None) }
            .map_err(|e| format!("WASAPI loopback failed to activate audio client: {e}"))?;

        let mix_format_ptr = unsafe { audio_client.GetMixFormat() }
            .map_err(|e| format!("WASAPI loopback failed to get mix format: {e}"))?;
        if mix_format_ptr.is_null() {
            return Err("WASAPI loopback returned null mix format".to_string());
        }

        unsafe {
            audio_client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_LOOPBACK,
                0,
                0,
                mix_format_ptr,
                None,
            )
        }
        .map_err(|e| format!("WASAPI loopback audio client initialization failed: {e}"))?;

        let (format_bytes, block_align) = unsafe {
            let format = *mix_format_ptr;
            let total_bytes = std::mem::size_of::<WAVEFORMATEX>() + usize::from(format.cbSize);
            let block_align = usize::from(format.nBlockAlign);
            let bytes =
                std::slice::from_raw_parts(mix_format_ptr as *const u8, total_bytes).to_vec();
            CoTaskMemFree(Some(mix_format_ptr as *const std::ffi::c_void));
            (bytes, block_align)
        };
        if block_align == 0 {
            return Err("WASAPI loopback returned invalid block alignment (0)".to_string());
        }

        let capture_client: IAudioCaptureClient = unsafe { audio_client.GetService() }
            .map_err(|e| format!("WASAPI loopback failed to get capture service: {e}"))?;

        let mut wav_writer = WavWriter::create(&output_path, &format_bytes)?;

        unsafe { audio_client.Start() }
            .map_err(|e| format!("WASAPI loopback failed to start audio stream: {e}"))?;

        if ready_tx.send(Ok(())).is_err() {
            let _ = unsafe { audio_client.Stop() };
            let _ = wav_writer.finalize();
            return Err("WASAPI loopback startup channel closed unexpectedly".to_string());
        }

        let capture_result = (|| -> Result<(), String> {
            let mut silence = Vec::<u8>::new();

            while !stop_flag.load(Ordering::Relaxed) {
                let mut packet_frames = unsafe { capture_client.GetNextPacketSize() }
                    .map_err(|e| format!("WASAPI loopback failed to read packet size: {e}"))?;

                if packet_frames == 0 {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                }

                while packet_frames > 0 {
                    let mut data_ptr: *mut u8 = ptr::null_mut();
                    let mut frame_count = 0u32;
                    let mut flags = 0u32;
                    unsafe {
                        capture_client.GetBuffer(
                            &mut data_ptr,
                            &mut frame_count,
                            &mut flags,
                            None,
                            None,
                        )
                    }
                    .map_err(|e| format!("WASAPI loopback failed to get audio buffer: {e}"))?;

                    let byte_count = usize::try_from(frame_count)
                        .unwrap_or(0)
                        .saturating_mul(block_align);
                    let write_result = if (flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0
                        || data_ptr.is_null()
                        || byte_count == 0
                    {
                        if silence.len() < byte_count {
                            silence.resize(byte_count, 0);
                        }
                        wav_writer.write_samples(&silence[..byte_count])
                    } else {
                        let bytes = unsafe {
                            std::slice::from_raw_parts(data_ptr as *const u8, byte_count)
                        };
                        wav_writer.write_samples(bytes)
                    };

                    let release_result = unsafe { capture_client.ReleaseBuffer(frame_count) }
                        .map_err(|e| {
                            format!("WASAPI loopback failed to release audio buffer: {e}")
                        });

                    write_result?;
                    release_result?;

                    packet_frames = unsafe { capture_client.GetNextPacketSize() }
                        .map_err(|e| format!("WASAPI loopback failed to read packet size: {e}"))?;
                }
            }

            Ok(())
        })();

        if let Err(err) = unsafe { audio_client.Stop() } {
            log::warn!("WASAPI loopback stream stop returned an error: {err}");
        }
        let finalize_result = wav_writer.finalize();

        capture_result?;
        finalize_result?;
        Ok(())
    };

    match run() {
        Ok(()) => Ok(()),
        Err(err) => {
            let _ = ready_tx.send(Err(err.clone()));
            Err(err)
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn run_loopback_capture_thread(
    _output_path: PathBuf,
    _stop_flag: Arc<AtomicBool>,
    ready_tx: mpsc::SyncSender<Result<(), String>>,
) -> Result<(), String> {
    let err = "WASAPI loopback capture is only available on Windows".to_string();
    let _ = ready_tx.send(Err(err.clone()));
    Err(err)
}

struct WavWriter {
    file: File,
    riff_size_offset: u64,
    data_size_offset: u64,
    written_data_bytes: u64,
}

impl WavWriter {
    fn create(path: &Path, format_bytes: &[u8]) -> Result<Self, String> {
        let mut file = File::create(path).map_err(|e| {
            format!(
                "Failed to create loopback audio file '{}': {e}",
                path.display()
            )
        })?;

        file.write_all(b"RIFF")
            .map_err(|e| format!("Failed to write WAV RIFF header: {e}"))?;
        let riff_size_offset = file
            .stream_position()
            .map_err(|e| format!("Failed to seek WAV header: {e}"))?;
        file.write_all(&0u32.to_le_bytes())
            .map_err(|e| format!("Failed to reserve WAV RIFF size: {e}"))?;
        file.write_all(b"WAVE")
            .map_err(|e| format!("Failed to write WAV signature: {e}"))?;

        file.write_all(b"fmt ")
            .map_err(|e| format!("Failed to write WAV fmt tag: {e}"))?;
        let fmt_len_u32 = u32::try_from(format_bytes.len())
            .map_err(|_| "WAV format block is too large".to_string())?;
        file.write_all(&fmt_len_u32.to_le_bytes())
            .map_err(|e| format!("Failed to write WAV fmt size: {e}"))?;
        file.write_all(format_bytes)
            .map_err(|e| format!("Failed to write WAV format block: {e}"))?;
        if format_bytes.len() % 2 != 0 {
            file.write_all(&[0u8])
                .map_err(|e| format!("Failed to write WAV fmt padding: {e}"))?;
        }

        file.write_all(b"data")
            .map_err(|e| format!("Failed to write WAV data tag: {e}"))?;
        let data_size_offset = file
            .stream_position()
            .map_err(|e| format!("Failed to seek WAV data header: {e}"))?;
        file.write_all(&0u32.to_le_bytes())
            .map_err(|e| format!("Failed to reserve WAV data size: {e}"))?;

        Ok(Self {
            file,
            riff_size_offset,
            data_size_offset,
            written_data_bytes: 0,
        })
    }

    fn write_samples(&mut self, data: &[u8]) -> Result<(), String> {
        if data.is_empty() {
            return Ok(());
        }
        self.file
            .write_all(data)
            .map_err(|e| format!("Failed to write loopback audio samples: {e}"))?;
        self.written_data_bytes = self.written_data_bytes.saturating_add(data.len() as u64);
        Ok(())
    }

    fn finalize(&mut self) -> Result<(), String> {
        let file_len = self
            .file
            .seek(SeekFrom::End(0))
            .map_err(|e| format!("Failed to finalize WAV size: {e}"))?;

        let riff_size = file_len.saturating_sub(8).min(u32::MAX as u64) as u32;
        let data_size = self.written_data_bytes.min(u32::MAX as u64) as u32;

        self.file
            .seek(SeekFrom::Start(self.riff_size_offset))
            .map_err(|e| format!("Failed to patch WAV RIFF size: {e}"))?;
        self.file
            .write_all(&riff_size.to_le_bytes())
            .map_err(|e| format!("Failed to write WAV RIFF size: {e}"))?;

        self.file
            .seek(SeekFrom::Start(self.data_size_offset))
            .map_err(|e| format!("Failed to patch WAV data size: {e}"))?;
        self.file
            .write_all(&data_size.to_le_bytes())
            .map_err(|e| format!("Failed to write WAV data size: {e}"))?;

        self.file
            .flush()
            .map_err(|e| format!("Failed to flush WAV file: {e}"))?;
        Ok(())
    }
}
