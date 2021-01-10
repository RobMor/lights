use libflac_sys::*;
use std::ffi::c_void;

pub struct FlacDecoder {
    inner: *mut FLAC__StreamDecoder,
}

impl FlacDecoder {
    pub fn new() -> FlacDecoder {
        let decoder = unsafe {
            let decoder: *mut FLAC__StreamDecoder = FLAC__stream_decoder_new();

            if decoder.is_null() {
                panic!("Failed to allocate StreamDecoder");
            }

            decoder
        };

        let status = unsafe {
            FLAC__stream_decoder_init_stream(
                decoder,
                Some(read_callback),
                None,
                None,
                None,
                None,
                Some(write_callback),
                None,
                Some(error_callback),
            )
        };

        match status {
            FLAC__STREAM_DECODER_INIT_STATUS_OK => (),
            s => {
                panic!("Error initializing stream: {}", match status {
                    FLAC__STREAM_DECODER_INIT_STATUS_UNSUPPORTED_CONTAINER => "The library was not compiled with support for the given container format.",
                    FLAC__STREAM_DECODER_INIT_STATUS_INVALID_CALLBACKS => "A required callback was not supplied.",
                    FLAC__STREAM_DECODER_INIT_STATUS_MEMORY_ALLOCATION_ERROR => "An error occurred allocating memory.",
                    FLAC__STREAM_DECODER_INIT_STATUS_ERROR_OPENING_FILE => "fopen() failed in FLAC__stream_decoder_init_file() or FLAC__stream_decoder_init_ogg_file().",
                    FLAC__STREAM_DECODER_INIT_STATUS_ALREADY_INITIALIZED => "FLAC__stream_decoder_init_*() was called when the decoder was already initialized, usually because FLAC__stream_decoder_finish() was not called.",
                    _ => unreachable!(),
                })
            }
        }

        FlacDecoder {
            inner: decoder
        }
    }

    fn read_callback(&mut self, buffer: &mut [u8]) -> FLAC__StreamDecoderReadStatus {
        FLAC__STREAM_DECODER_READ_STATUS_CONTINUE
    }
}

unsafe extern "C" fn read_callback(decoder: *const FLAC__StreamDecoder, buffer: *mut FLAC__byte, bytes: *mut usize, client_data: *mut c_void) -> FLAC__StreamDecoderReadStatus {
    unsafe {
        let buffer = std::slice::from_raw_parts_mut(buffer, *bytes);
        (*std::mem::transmute::<*mut c_void, *mut FlacDecoder>(client_data)).read_callback(buffer)
    }
}

unsafe extern "C" fn write_callback(decoder: *const FLAC__StreamDecoder, frame: *const FLAC__Frame, buffer: *const *const FLAC__int32, client_data: *mut c_void) -> FLAC__StreamDecoderWriteStatus {
    
}