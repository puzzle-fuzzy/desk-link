use windows::{
    Win32::{
        Graphics::{Direct3D11::ID3D11Device, Dxgi::IDXGIDevice},
        Media::MediaFoundation::{
            IMFActivate, IMFTransform, MF_SA_D3D11_AWARE, MF_TRANSFORM_ASYNC, MFCreateAttributes,
            MFMediaType_Video, MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_ADAPTER_LUID,
            MFT_ENUM_FLAG_HARDWARE, MFT_ENUM_FLAG_SORTANDFILTER, MFT_REGISTER_TYPE_INFO, MFTEnum2,
            MFVideoFormat_H264, MFVideoFormat_NV12,
        },
        System::Com::CoTaskMemFree,
    },
    core::Interface,
};

use super::{EncoderError, native};

pub(super) fn hardware_candidates_for_device(
    device: &ID3D11Device,
) -> Result<(u64, Vec<IMFActivate>), EncoderError> {
    let fail = |stage: &'static str, error: windows::core::Error| {
        EncoderError::GpuOperation(super::GpuEncoderFailure {
            stage,
            hresult: Some(error.code().0),
        })
    };
    let dxgi = device
        .cast::<IDXGIDevice>()
        .map_err(|error| fail("get_dxgi_device", error))?;
    let adapter = unsafe { dxgi.GetAdapter() }.map_err(|error| fail("get_gpu_adapter", error))?;
    let description =
        unsafe { adapter.GetDesc() }.map_err(|error| fail("get_gpu_adapter_description", error))?;
    let luid = (u64::from(description.AdapterLuid.HighPart as u32) << 32)
        | u64::from(description.AdapterLuid.LowPart);
    let mut attributes = None;
    unsafe { MFCreateAttributes(&mut attributes, 1) }
        .map_err(|error| fail("create_gpu_enumeration_attributes", error))?;
    let attributes = attributes.ok_or({
        EncoderError::GpuOperation(super::GpuEncoderFailure {
            stage: "gpu_enumeration_attributes_null",
            hresult: None,
        })
    })?;
    unsafe { attributes.SetUINT64(&MFT_ENUM_ADAPTER_LUID, luid) }
        .map_err(|error| fail("set_gpu_adapter_luid_filter", error))?;
    let input = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_NV12,
    };
    let output = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_H264,
    };
    let mut values = std::ptr::null_mut();
    let mut count = 0;
    let result = unsafe {
        MFTEnum2(
            MFT_CATEGORY_VIDEO_ENCODER,
            MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
            Some(&input),
            Some(&output),
            &attributes,
            &mut values,
            &mut count,
        )
    };
    let activations = if values.is_null() {
        Vec::new()
    } else {
        let entries = unsafe { std::slice::from_raw_parts_mut(values, count as usize) };
        let activations = entries.iter_mut().filter_map(Option::take).collect();
        unsafe { CoTaskMemFree(Some(values.cast())) };
        activations
    };
    result.map_err(|error| fail("enumerate_gpu_adapter_mfts", error))?;
    Ok((luid, activations))
}

/// Read-only MFT attributes are only capability evidence, not successful GPU
/// manager negotiation, surface sample admission or encoded output evidence.
#[derive(Clone, Debug, serde::Serialize)]
pub struct HardwareEncoderCapability {
    pub name: String,
    pub activated: bool,
    pub activation_hresult: Option<i32>,
    pub attributes_hresult: Option<i32>,
    pub asynchronous: Option<bool>,
    pub d3d11_aware: Option<bool>,
    pub d3d11_attribute_hresult: Option<i32>,
}

pub fn probe_hardware_encoder_capabilities() -> Result<Vec<HardwareEncoderCapability>, EncoderError>
{
    let _runtime = native::NativeRuntime::start()?;
    let candidates = native::hardware_candidates()?;
    let mut capabilities = Vec::new();
    for activation in candidates.iter().take(16) {
        let mut result = HardwareEncoderCapability {
            name: native::activation_name(activation),
            activated: false,
            activation_hresult: None,
            attributes_hresult: None,
            asynchronous: None,
            d3d11_aware: None,
            d3d11_attribute_hresult: None,
        };
        match unsafe { activation.ActivateObject::<IMFTransform>() } {
            Ok(transform) => {
                result.activated = true;
                match unsafe { transform.GetAttributes() } {
                    Ok(attributes) => {
                        result.asynchronous = unsafe { attributes.GetUINT32(&MF_TRANSFORM_ASYNC) }
                            .ok()
                            .map(|value| value != 0);
                        match unsafe { attributes.GetUINT32(&MF_SA_D3D11_AWARE) } {
                            Ok(value) => result.d3d11_aware = Some(value != 0),
                            Err(error) => result.d3d11_attribute_hresult = Some(error.code().0),
                        }
                    }
                    Err(error) => result.attributes_hresult = Some(error.code().0),
                }
            }
            Err(error) => result.activation_hresult = Some(error.code().0),
        }
        let _ = unsafe { activation.ShutdownObject() };
        capabilities.push(result);
    }
    Ok(capabilities)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "explicit local hardware MFT capability inventory; no desktop capture"]
    fn actual_hardware_mft_d3d11_capability_inventory() {
        let capabilities = probe_hardware_encoder_capabilities().unwrap();
        println!("{}", serde_json::to_string(&capabilities).unwrap());
    }
}
