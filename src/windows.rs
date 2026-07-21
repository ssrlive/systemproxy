use crate::{Autoproxy, Error, Result, SystemProxy};
use std::ffi::{c_void, OsString};
use std::{
    mem::{size_of, ManuallyDrop},
    net::SocketAddr,
    os::windows::ffi::OsStringExt,
    ptr::{null, null_mut},
    str::FromStr,
};
use windows_sys::Win32::Foundation::{GetLastError, ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows_sys::Win32::Networking::WinInet::{
    InternetSetOptionW, INTERNET_OPTION_PER_CONNECTION_OPTION,
    INTERNET_OPTION_PROXY_SETTINGS_CHANGED, INTERNET_OPTION_REFRESH,
    INTERNET_PER_CONN_AUTOCONFIG_URL, INTERNET_PER_CONN_FLAGS, INTERNET_PER_CONN_OPTIONW,
    INTERNET_PER_CONN_OPTIONW_0, INTERNET_PER_CONN_OPTION_LISTW, INTERNET_PER_CONN_PROXY_BYPASS,
    INTERNET_PER_CONN_PROXY_SERVER, PROXY_TYPE_AUTO_DETECT, PROXY_TYPE_AUTO_PROXY_URL,
    PROXY_TYPE_DIRECT, PROXY_TYPE_PROXY,
};
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER, KEY_READ, REG_DWORD,
    REG_EXPAND_SZ, REG_SZ,
};

const SUB_KEY: &str = "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Internet Settings";

fn last_os_error() -> std::io::Error {
    std::io::Error::from_raw_os_error(unsafe { GetLastError() } as i32)
}

fn to_wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

fn read_registry_dword_value(key: HKEY, name: &str) -> Result<Option<u32>> {
    let name_w = to_wide(name);
    let mut reg_type = 0u32;
    let mut data = 0u32;
    let mut data_len = size_of::<u32>() as u32;

    let status = unsafe {
        RegQueryValueExW(
            key,
            name_w.as_ptr(),
            null(),
            &mut reg_type,
            &mut data as *mut _ as *mut u8,
            &mut data_len,
        )
    };

    if status == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    if status != ERROR_SUCCESS {
        return Err(Error::Io(last_os_error()));
    }
    if reg_type != REG_DWORD {
        return Err(Error::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "registry value is not a DWORD",
        )));
    }

    Ok(Some(data))
}

fn read_registry_string_value(key: HKEY, name: &str) -> Result<Option<String>> {
    let name_w = to_wide(name);
    let mut reg_type = 0u32;
    let mut data_len = 0u32;

    let status = unsafe {
        RegQueryValueExW(
            key,
            name_w.as_ptr(),
            null(),
            &mut reg_type,
            null_mut(),
            &mut data_len,
        )
    };

    if status == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    if status != ERROR_SUCCESS {
        return Err(Error::Io(last_os_error()));
    }
    if reg_type != REG_SZ && reg_type != REG_EXPAND_SZ {
        return Err(Error::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "registry value is not a string",
        )));
    }
    if data_len == 0 {
        return Ok(Some(String::new()));
    }

    let mut buffer = vec![0u8; data_len as usize];
    let status = unsafe {
        RegQueryValueExW(
            key,
            name_w.as_ptr(),
            null_mut(),
            &mut reg_type,
            buffer.as_mut_ptr(),
            &mut data_len,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(Error::Io(last_os_error()));
    }
    if !data_len.is_multiple_of(2) {
        return Err(Error::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "invalid unicode string length",
        )));
    }

    let wide_chars = unsafe {
        std::slice::from_raw_parts(buffer.as_ptr() as *const u16, (data_len / 2) as usize)
    };
    let wide_chars = if let Some(pos) = wide_chars.iter().position(|&c| c == 0) {
        &wide_chars[..pos]
    } else {
        wide_chars
    };
    let value = OsString::from_wide(wide_chars)
        .to_string_lossy()
        .into_owned();
    Ok(Some(value))
}

fn open_internet_settings_key() -> Result<HKEY> {
    let mut hkey: HKEY = 0 as _;
    let sub_key = to_wide(SUB_KEY);

    let status =
        unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, sub_key.as_ptr(), 0, KEY_READ, &mut hkey) };
    if status != ERROR_SUCCESS {
        return Err(Error::Io(last_os_error()));
    }
    Ok(hkey)
}

fn close_registry_key(key: HKEY) {
    if key != 0 as _ {
        unsafe { RegCloseKey(key) };
    }
}

/// unset proxy
fn unset_proxy() -> Result<()> {
    let mut p_opts = ManuallyDrop::new(Vec::<INTERNET_PER_CONN_OPTIONW>::with_capacity(1));
    p_opts.push(INTERNET_PER_CONN_OPTIONW {
        dwOption: INTERNET_PER_CONN_FLAGS,
        Value: {
            let mut v = INTERNET_PER_CONN_OPTIONW_0::default();
            v.dwValue = PROXY_TYPE_DIRECT;
            v
        },
    });
    let opts = INTERNET_PER_CONN_OPTION_LISTW {
        dwSize: size_of::<INTERNET_PER_CONN_OPTION_LISTW>() as u32,
        dwOptionCount: 1,
        dwOptionError: 0,
        pOptions: p_opts.as_mut_ptr(),
        pszConnection: null_mut(),
    };
    let res = apply(&opts);
    unsafe {
        ManuallyDrop::drop(&mut p_opts);
    }
    res
}

fn set_auto_proxy(server: String) -> Result<()> {
    let mut p_opts = ManuallyDrop::new(Vec::<INTERNET_PER_CONN_OPTIONW>::with_capacity(2));
    p_opts.push(INTERNET_PER_CONN_OPTIONW {
        dwOption: INTERNET_PER_CONN_FLAGS,
        Value: INTERNET_PER_CONN_OPTIONW_0 {
            dwValue: PROXY_TYPE_AUTO_DETECT | PROXY_TYPE_AUTO_PROXY_URL | PROXY_TYPE_DIRECT,
        },
    });

    let mut s = ManuallyDrop::new(
        server
            .encode_utf16()
            .chain(Some(0u16))
            .collect::<Vec<u16>>(),
    );
    p_opts.push(INTERNET_PER_CONN_OPTIONW {
        dwOption: INTERNET_PER_CONN_AUTOCONFIG_URL,
        Value: INTERNET_PER_CONN_OPTIONW_0 {
            pszValue: s.as_ptr() as *mut u16,
        },
    });

    let opts = INTERNET_PER_CONN_OPTION_LISTW {
        dwSize: size_of::<INTERNET_PER_CONN_OPTION_LISTW>() as u32,
        dwOptionCount: 2,
        dwOptionError: 0,
        pOptions: p_opts.as_mut_ptr(),
        pszConnection: null_mut(),
    };

    let res = apply(&opts);
    unsafe {
        ManuallyDrop::drop(&mut s);
        ManuallyDrop::drop(&mut p_opts);
    }
    res
}

/// set global proxy
fn set_global_proxy(server: String, bypass: String) -> Result<()> {
    let mut p_opts = ManuallyDrop::new(Vec::<INTERNET_PER_CONN_OPTIONW>::with_capacity(3));
    p_opts.push(INTERNET_PER_CONN_OPTIONW {
        dwOption: INTERNET_PER_CONN_FLAGS,
        Value: INTERNET_PER_CONN_OPTIONW_0 {
            dwValue: PROXY_TYPE_PROXY | PROXY_TYPE_DIRECT,
        },
    });

    let mut s = ManuallyDrop::new(
        server
            .encode_utf16()
            .chain(Some(0u16))
            .collect::<Vec<u16>>(),
    );
    p_opts.push(INTERNET_PER_CONN_OPTIONW {
        dwOption: INTERNET_PER_CONN_PROXY_SERVER,
        Value: INTERNET_PER_CONN_OPTIONW_0 {
            pszValue: s.as_ptr() as *mut u16,
        },
    });

    let mut b = ManuallyDrop::new(
        bypass
            .clone()
            .encode_utf16()
            .chain(Some(0u16))
            .collect::<Vec<u16>>(),
    );
    p_opts.push(INTERNET_PER_CONN_OPTIONW {
        dwOption: INTERNET_PER_CONN_PROXY_BYPASS,
        Value: INTERNET_PER_CONN_OPTIONW_0 {
            pszValue: b.as_ptr() as *mut u16,
        },
    });

    let opts = INTERNET_PER_CONN_OPTION_LISTW {
        dwSize: size_of::<INTERNET_PER_CONN_OPTION_LISTW>() as u32,
        dwOptionCount: 3,
        dwOptionError: 0,
        pOptions: p_opts.as_mut_ptr(),
        pszConnection: null_mut(),
    };

    let res = apply(&opts);
    unsafe {
        ManuallyDrop::drop(&mut s);
        ManuallyDrop::drop(&mut b);
        ManuallyDrop::drop(&mut p_opts);
    }
    res
}

fn apply(options: &INTERNET_PER_CONN_OPTION_LISTW) -> Result<()> {
    unsafe {
        if InternetSetOptionW(
            null(),
            INTERNET_OPTION_PER_CONNECTION_OPTION,
            options as *const INTERNET_PER_CONN_OPTION_LISTW as *const c_void,
            size_of::<INTERNET_PER_CONN_OPTION_LISTW>() as u32,
        ) == 0
        {
            return Err(Error::Io(last_os_error()));
        }
        if InternetSetOptionW(null(), INTERNET_OPTION_PROXY_SETTINGS_CHANGED, null(), 0) == 0 {
            return Err(Error::Io(last_os_error()));
        }
        if InternetSetOptionW(null(), INTERNET_OPTION_REFRESH, null(), 0) == 0 {
            return Err(Error::Io(last_os_error()));
        }
    }
    Ok(())
}

impl SystemProxy {
    pub fn get_system_proxy() -> Result<SystemProxy> {
        let hkcu = open_internet_settings_key()?;
        let result = (|| {
            let enable =
                read_registry_dword_value(hkcu, "ProxyEnable")?.unwrap_or_default() == 1u32;
            let server = read_registry_string_value(hkcu, "ProxyServer")?.unwrap_or_default();
            let (host, port) = if server.is_empty() {
                ("".into(), 0)
            } else {
                let socket = SocketAddr::from_str(server.as_str())
                    .or(Err(Error::ParseStr(server.to_string())))?;
                let host = socket.ip().to_string();
                let port = socket.port();
                (host, port)
            };
            let bypass = read_registry_string_value(hkcu, "ProxyOverride")?.unwrap_or_default();
            Ok(SystemProxy {
                enable,
                host,
                port,
                bypass,
            })
        })();
        close_registry_key(hkcu);
        result
    }

    pub fn set_system_proxy(&self) -> Result<()> {
        match self.enable {
            true => set_global_proxy(format!("{}:{}", self.host, self.port), self.bypass.clone()),
            false => unset_proxy(),
        }
    }
}

impl Autoproxy {
    pub fn get_auto_proxy() -> Result<Autoproxy> {
        let hkcu = open_internet_settings_key()?;
        let result = (|| {
            let url = read_registry_string_value(hkcu, "AutoConfigURL")?.unwrap_or_default();
            let enable = !url.is_empty();
            Ok(Autoproxy { enable, url })
        })();
        close_registry_key(hkcu);
        result
    }

    pub fn set_auto_proxy(&self) -> Result<()> {
        match self.enable {
            true => set_auto_proxy(self.url.clone()),
            false => unset_proxy(),
        }
    }
}
