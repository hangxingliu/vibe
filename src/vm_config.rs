//! Virtualization.framework configuration for the interactive VM: EFI boot,
//! disk, VirtioFS shares, network device, and serial ports.

use std::{
    env, io,
    os::unix::io::{IntoRawFd, OwnedFd},
    path::Path,
};

use objc2::{AnyThread, rc::Retained, runtime::ProtocolObject};
use objc2_foundation::*;
use objc2_virtualization::*;

use crate::DirectoryShare;
use crate::networking::{PreparedNetworkBackend, USERNET_MAC_ADDRESS};

pub(crate) const SHARED_DIRECTORIES_TAG: &str = "shared";

#[allow(clippy::too_many_arguments)]
pub(crate) fn create_vm_configuration(
    disk_path: &Path,
    directory_shares: &[DirectoryShare],
    network_backend: &mut PreparedNetworkBackend,
    vm_reads_from_fd: OwnedFd,
    vm_writes_to_fd: OwnedFd,
    resize_reads_from_fd: OwnedFd,
    cpu_count: usize,
    ram_bytes: u64,
) -> Result<Retained<VZVirtualMachineConfiguration>, Box<dyn std::error::Error>> {
    unsafe {
        let platform =
            VZGenericPlatformConfiguration::init(VZGenericPlatformConfiguration::alloc());

        let boot_loader = VZEFIBootLoader::init(VZEFIBootLoader::alloc());
        let variable_store = load_efi_variable_store()?;
        boot_loader.setVariableStore(Some(&variable_store));

        let config = VZVirtualMachineConfiguration::new();
        config.setPlatform(&platform);
        config.setBootLoader(Some(&boot_loader));
        config.setCPUCount(cpu_count as NSUInteger);
        config.setMemorySize(ram_bytes);

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

                    let network_fd = vm_socket_fd
                        .take()
                        .ok_or_else(|| io::Error::other("vibe-usernet socket already consumed"))?;
                    let file_handle = NSFileHandle::initWithFileDescriptor_closeOnDealloc(
                        NSFileHandle::alloc(),
                        network_fd.into_raw_fd(),
                        true,
                    );
                    let attachment = VZFileHandleNetworkDeviceAttachment::initWithFileHandle(
                        VZFileHandleNetworkDeviceAttachment::alloc(),
                        &file_handle,
                    );
                    network_device.setAttachment(Some(&attachment));
                }
            }
            Retained::into_super(network_device)
        }]));

        config.setEntropyDevices(&NSArray::from_retained_slice(&[Retained::into_super(
            VZVirtioEntropyDeviceConfiguration::new(),
        )]));

        ////////////////////////////
        // Disks
        {
            let disk_attachment = VZDiskImageStorageDeviceAttachment::initWithURL_readOnly_cachingMode_synchronizationMode_error(
                VZDiskImageStorageDeviceAttachment::alloc(),
                &nsurl_from_path(disk_path).unwrap(),
                false,
                VZDiskImageCachingMode::Cached,
                VZDiskImageSynchronizationMode::Full,
            ).unwrap();

            let disk_device = VZVirtioBlockDeviceConfiguration::initWithAttachment(
                VZVirtioBlockDeviceConfiguration::alloc(),
                &disk_attachment,
            );

            let storage_devices: Retained<NSArray<_>> =
                NSArray::from_retained_slice(&[Retained::into_super(disk_device)]);

            config.setStorageDevices(&storage_devices);
        };

        ////////////////////////////
        // Directory shares

        if !directory_shares.is_empty() {
            let directories: Retained<NSMutableDictionary<NSString, VZSharedDirectory>> =
                NSMutableDictionary::new();

            for share in directory_shares {
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

                let key = NSString::from_str(&share.tag());
                directories.setObject_forKey(&*shared_directory, ProtocolObject::from_ref(&*key));
            }

            let multi_share = VZMultipleDirectoryShare::initWithDirectories(
                VZMultipleDirectoryShare::alloc(),
                &directories,
            );

            let device = VZVirtioFileSystemDeviceConfiguration::initWithTag(
                VZVirtioFileSystemDeviceConfiguration::alloc(),
                &NSString::from_str(SHARED_DIRECTORIES_TAG),
            );
            device.setShare(Some(&multi_share));

            let share_devices = NSArray::from_retained_slice(&[device.into_super()]);
            config.setDirectorySharingDevices(&share_devices);
        }

        ////////////////////////////
        // Serial ports
        {
            let ns_read_handle = NSFileHandle::initWithFileDescriptor_closeOnDealloc(
                NSFileHandle::alloc(),
                vm_reads_from_fd.into_raw_fd(),
                true,
            );

            let ns_write_handle = NSFileHandle::initWithFileDescriptor_closeOnDealloc(
                NSFileHandle::alloc(),
                vm_writes_to_fd.into_raw_fd(),
                true,
            );

            let serial_attach =
                VZFileHandleSerialPortAttachment::initWithFileHandleForReading_fileHandleForWriting(
                    VZFileHandleSerialPortAttachment::alloc(),
                    Some(&ns_read_handle),
                    Some(&ns_write_handle),
                );
            let serial_port = VZVirtioConsoleDeviceSerialPortConfiguration::new();
            serial_port.setAttachment(Some(&serial_attach));

            let resize_read_handle = NSFileHandle::initWithFileDescriptor_closeOnDealloc(
                NSFileHandle::alloc(),
                resize_reads_from_fd.into_raw_fd(),
                true,
            );
            let resize_attach =
                VZFileHandleSerialPortAttachment::initWithFileHandleForReading_fileHandleForWriting(
                    VZFileHandleSerialPortAttachment::alloc(),
                    Some(&resize_read_handle),
                    None,
                );
            let resize_port = VZVirtioConsoleDeviceSerialPortConfiguration::new();
            resize_port.setAttachment(Some(&resize_attach));

            let serial_ports: Retained<NSArray<_>> = NSArray::from_retained_slice(&[
                Retained::into_super(serial_port),
                Retained::into_super(resize_port),
            ]);

            config.setSerialPorts(&serial_ports);
        }

        ////////////////////////////
        // Validate
        config.validateWithError().map_err(|e| {
            io::Error::other(format!(
                "Invalid VM configuration: {:?}",
                e.localizedDescription()
            ))
        })?;

        Ok(config)
    }
}

fn load_efi_variable_store() -> Result<Retained<VZEFIVariableStore>, Box<dyn std::error::Error>> {
    unsafe {
        let temp_dir = std::env::temp_dir();
        let temp_path = temp_dir.join(format!("efi_variable_store_{}.efivars", std::process::id()));
        let url = nsurl_from_path(&temp_path)?;
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
