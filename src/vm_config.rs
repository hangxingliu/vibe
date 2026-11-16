//! Virtualization.framework configuration.
//!
//! Interactive Debian VMs are built with [`default_interactive_spec`], which
//! matches the historical single-disk layout. Maintenance VMs pass their own
//! [`VmSpec`]: several disks (read-only when asked), no network, and serial
//! ports chosen by the caller. Boot is EFI only.

use std::{
    env, io,
    os::unix::io::{IntoRawFd, OwnedFd},
    path::{Path, PathBuf},
};

use objc2::{AnyThread, rc::Retained, runtime::ProtocolObject};
use objc2_foundation::*;
use objc2_virtualization::*;

use crate::networking::{PreparedNetworkBackend, USERNET_MAC_ADDRESS};
use crate::shares::DirectoryShare;
use crate::vm_io::create_pipe;

pub(crate) const SHARED_DIRECTORIES_TAG: &str = "shared";

/// Why a serial port exists. The role is a label for the caller; port order
/// alone decides the guest device (`hvc0`, `hvc1`, ...).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SerialRole {
    /// Interactive console. Debian `hvc0`.
    Terminal,
    /// Terminal size updates. Debian `hvc1`.
    Resize,
    /// Maintenance console log. Maintenance `hvc0`.
    Log,
    /// Maintenance control frames. Maintenance `hvc1`.
    Control,
}

impl SerialRole {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            SerialRole::Terminal => "terminal",
            SerialRole::Resize => "resize",
            SerialRole::Log => "log",
            SerialRole::Control => "control",
        }
    }
}

/// One virtio-blk disk. Vec order is guest order: index 0 is `vda`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiskSpec {
    pub(crate) path: PathBuf,
    pub(crate) read_only: bool,
}

/// A directory exported by a VirtioFS device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SharedDirectory {
    /// Name inside a multiple-directory share. Ignored for a single-directory device.
    pub(crate) name: String,
    pub(crate) host: PathBuf,
    pub(crate) read_only: bool,
}

/// What one VirtioFS device exports.
pub(crate) enum ShareBody {
    /// `VZMultipleDirectoryShare`. The interactive `shared` device uses this.
    Multiple(Vec<SharedDirectory>),
    /// `VZSingleDirectoryShare`. Currently unused.
    #[allow(dead_code)]
    Single(SharedDirectory),
}

pub(crate) struct ShareDevice {
    pub(crate) tag: String,
    pub(crate) body: ShareBody,
}

/// One virtio console port.
pub(crate) struct SerialPort {
    pub(crate) role: SerialRole,
    /// Framework reads this and delivers the bytes to the guest.
    pub(crate) read_fd: Option<OwnedFd>,
    /// Framework writes guest output to this.
    pub(crate) write_fd: Option<OwnedFd>,
    /// Host end of `write_fd`'s socket. Not installed in the VM.
    /// [`crate::vm_lifecycle::run_headless`] takes it for the log and control ports.
    pub(crate) host_read: Option<OwnedFd>,
}

/// Full VM configuration before it is turned into a framework object.
pub(crate) struct VmSpec<'a> {
    pub(crate) disks: Vec<DiskSpec>,
    pub(crate) network: Option<&'a mut PreparedNetworkBackend>,
    pub(crate) share_devices: Vec<ShareDevice>,
    pub(crate) serial_ports: Vec<SerialPort>,
    pub(crate) efi_vars: PathBuf,
    pub(crate) cpu_count: usize,
    pub(crate) ram_bytes: u64,
}

/// Serial port that captures guest output and does not feed the guest input.
/// The host read end is stored on the port for `run_headless`.
pub(crate) fn output_serial(role: SerialRole) -> SerialPort {
    let (host_read, guest_write) = create_pipe();
    SerialPort {
        role,
        read_fd: None,
        write_fd: Some(guest_write),
        host_read: Some(host_read),
    }
}

/// The interactive Debian configuration: one writable disk, the caller's
/// network backend, the `shared` multiple-directory device when there are
/// shares, `hvc0` terminal, and `hvc1` resize.
#[allow(clippy::too_many_arguments)]
pub(crate) fn default_interactive_spec<'a>(
    disk_path: &Path,
    directory_shares: &[DirectoryShare],
    network: &'a mut PreparedNetworkBackend,
    vm_reads_from: OwnedFd,
    vm_writes_to: OwnedFd,
    resize_reads_from: OwnedFd,
    cpu_count: usize,
    ram_bytes: u64,
) -> VmSpec<'a> {
    let mut share_devices = Vec::new();
    if !directory_shares.is_empty() {
        share_devices.push(ShareDevice {
            tag: SHARED_DIRECTORIES_TAG.to_string(),
            body: ShareBody::Multiple(
                directory_shares
                    .iter()
                    .map(|share| SharedDirectory {
                        name: share.tag(),
                        host: share.host.clone(),
                        read_only: share.read_only,
                    })
                    .collect(),
            ),
        });
    }

    VmSpec {
        disks: vec![DiskSpec {
            path: disk_path.to_path_buf(),
            read_only: false,
        }],
        network: Some(network),
        share_devices,
        serial_ports: vec![
            SerialPort {
                role: SerialRole::Terminal,
                read_fd: Some(vm_reads_from),
                write_fd: Some(vm_writes_to),
                host_read: None,
            },
            SerialPort {
                role: SerialRole::Resize,
                read_fd: Some(resize_reads_from),
                write_fd: None,
                host_read: None,
            },
        ],
        efi_vars: default_efi_vars_path(),
        cpu_count,
        ram_bytes,
    }
}

pub(crate) fn default_efi_vars_path() -> PathBuf {
    env::temp_dir().join(format!("efi_variable_store_{}.efivars", std::process::id()))
}

pub(crate) fn create_vm_configuration(
    spec: VmSpec<'_>,
) -> Result<Retained<VZVirtualMachineConfiguration>, Box<dyn std::error::Error>> {
    unsafe {
        let platform =
            VZGenericPlatformConfiguration::init(VZGenericPlatformConfiguration::alloc());

        let boot_loader = VZEFIBootLoader::init(VZEFIBootLoader::alloc());
        let variable_store = load_efi_variable_store(&spec.efi_vars)?;
        boot_loader.setVariableStore(Some(&variable_store));

        let config = VZVirtualMachineConfiguration::new();
        config.setPlatform(&platform);
        config.setBootLoader(Some(&boot_loader));
        config.setCPUCount(spec.cpu_count as NSUInteger);
        config.setMemorySize(spec.ram_bytes);

        if let Some(network_backend) = spec.network {
            config.setNetworkDevices(&NSArray::from_retained_slice(&[{
                let network_device = VZVirtioNetworkDeviceConfiguration::new();
                match network_backend {
                    PreparedNetworkBackend::VzNat => {
                        network_device.setAttachment(Some(&VZNATNetworkDeviceAttachment::new()));
                    }
                    PreparedNetworkBackend::Usernet { vm_socket_fd, .. } => {
                        let mac_address = VZMACAddress::initWithString(
                            VZMACAddress::alloc(),
                            &NSString::from_str(USERNET_MAC_ADDRESS),
                        )
                        .ok_or_else(|| io::Error::other("invalid usernet MAC address"))?;
                        network_device.setMACAddress(&mac_address);

                        let network_fd = vm_socket_fd.take().ok_or_else(|| {
                            io::Error::other("vibe-usernet socket already consumed")
                        })?;
                        let file_handle = ns_file_handle(network_fd);
                        let attachment = VZFileHandleNetworkDeviceAttachment::initWithFileHandle(
                            VZFileHandleNetworkDeviceAttachment::alloc(),
                            &file_handle,
                        );
                        network_device.setAttachment(Some(&attachment));
                    }
                }
                Retained::into_super(network_device)
            }]));
        }

        config.setEntropyDevices(&NSArray::from_retained_slice(&[Retained::into_super(
            VZVirtioEntropyDeviceConfiguration::new(),
        )]));

        let mut storage_devices = Vec::with_capacity(spec.disks.len());
        for disk in &spec.disks {
            let url = nsurl_from_path(&disk.path)?;
            let disk_attachment = VZDiskImageStorageDeviceAttachment::initWithURL_readOnly_cachingMode_synchronizationMode_error(
                VZDiskImageStorageDeviceAttachment::alloc(),
                &url,
                disk.read_only,
                VZDiskImageCachingMode::Cached,
                VZDiskImageSynchronizationMode::Full,
            )
            .map_err(|err| io::Error::other(disk_attachment_error(&disk.path, &err)))?;

            let disk_device = VZVirtioBlockDeviceConfiguration::initWithAttachment(
                VZVirtioBlockDeviceConfiguration::alloc(),
                &disk_attachment,
            );
            storage_devices.push(Retained::into_super(disk_device));
        }
        config.setStorageDevices(&NSArray::from_retained_slice(&storage_devices));

        if !spec.share_devices.is_empty() {
            let mut devices = Vec::with_capacity(spec.share_devices.len());
            for share_device in &spec.share_devices {
                let device = VZVirtioFileSystemDeviceConfiguration::initWithTag(
                    VZVirtioFileSystemDeviceConfiguration::alloc(),
                    &NSString::from_str(&share_device.tag),
                );
                match &share_device.body {
                    ShareBody::Multiple(shares) => {
                        let directories: Retained<
                            NSMutableDictionary<NSString, VZSharedDirectory>,
                        > = NSMutableDictionary::new();
                        for share in shares {
                            assert!(
                                share.host.is_dir(),
                                "path does not exist or is not a directory: {:?}",
                                share.host
                            );
                            let url = nsurl_from_path(&share.host)?;
                            let shared_directory = VZSharedDirectory::initWithURL_readOnly(
                                VZSharedDirectory::alloc(),
                                &url,
                                share.read_only,
                            );
                            let key = NSString::from_str(&share.name);
                            directories.setObject_forKey(
                                &*shared_directory,
                                ProtocolObject::from_ref(&*key),
                            );
                        }
                        let multi_share = VZMultipleDirectoryShare::initWithDirectories(
                            VZMultipleDirectoryShare::alloc(),
                            &directories,
                        );
                        device.setShare(Some(&multi_share));
                    }
                    ShareBody::Single(share) => {
                        assert!(
                            share.host.is_dir(),
                            "path does not exist or is not a directory: {:?}",
                            share.host
                        );
                        let url = nsurl_from_path(&share.host)?;
                        let shared_directory = VZSharedDirectory::initWithURL_readOnly(
                            VZSharedDirectory::alloc(),
                            &url,
                            share.read_only,
                        );
                        let single = VZSingleDirectoryShare::initWithDirectory(
                            VZSingleDirectoryShare::alloc(),
                            &shared_directory,
                        );
                        device.setShare(Some(&single));
                    }
                }
                devices.push(device.into_super());
            }
            config.setDirectorySharingDevices(&NSArray::from_retained_slice(&devices));
        }

        let mut serial_ports = Vec::with_capacity(spec.serial_ports.len());
        for port in spec.serial_ports {
            // `port.role` is a label. Guest device order is the Vec order.
            let read_handle = port.read_fd.map(ns_file_handle);
            let write_handle = port.write_fd.map(ns_file_handle);
            let serial_attach =
                VZFileHandleSerialPortAttachment::initWithFileHandleForReading_fileHandleForWriting(
                    VZFileHandleSerialPortAttachment::alloc(),
                    read_handle.as_deref(),
                    write_handle.as_deref(),
                );
            let serial_port = VZVirtioConsoleDeviceSerialPortConfiguration::new();
            serial_port.setAttachment(Some(&serial_attach));
            serial_ports.push(Retained::into_super(serial_port));
        }
        config.setSerialPorts(&NSArray::from_retained_slice(&serial_ports));

        config.validateWithError().map_err(|err| {
            io::Error::other(format!(
                "Invalid VM configuration: {:?}",
                err.localizedDescription()
            ))
        })?;

        Ok(config)
    }
}

fn ns_file_handle(fd: OwnedFd) -> Retained<NSFileHandle> {
    NSFileHandle::initWithFileDescriptor_closeOnDealloc(
        NSFileHandle::alloc(),
        fd.into_raw_fd(),
        true,
    )
}

/// A missing disk fails in `initWithURL:readOnly:...:error:`, before
/// `validateWithError`. Surface that as a configuration error instead of panicking.
fn disk_attachment_error(path: &Path, err: &NSError) -> String {
    let desc = err.localizedDescription();
    if !path.exists() {
        format!(
            "Invalid VM configuration: disk image does not exist: {} ({desc})",
            path.display()
        )
    } else {
        format!(
            "Invalid VM configuration: failed to attach disk image {}: {desc}",
            path.display()
        )
    }
}

fn load_efi_variable_store(
    path: &Path,
) -> Result<Retained<VZEFIVariableStore>, Box<dyn std::error::Error>> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    unsafe {
        let url = nsurl_from_path(path)?;
        let options = VZEFIVariableStoreInitializationOptions::AllowOverwrite;
        let store = VZEFIVariableStore::initCreatingVariableStoreAtURL_options_error(
            VZEFIVariableStore::alloc(),
            &url,
            options,
        )?;
        Ok(store)
    }
}

pub(crate) fn nsurl_from_path(path: &Path) -> Result<Retained<NSURL>, Box<dyn std::error::Error>> {
    let abs_path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir()?.join(path)
    };
    let ns_path = NSString::from_str(
        abs_path
            .to_str()
            .ok_or("Non-UTF8 path encountered while building NSURL")?,
    );
    Ok(NSURL::fileURLWithPath(&ns_path))
}
