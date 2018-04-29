#![feature(lang_items)]
#![no_main]
#![no_std]


extern crate efi;
extern crate xmas_elf;


use core::{
    fmt,
    mem,
    slice,
};
use efi::{
    boot_services,
    boot_services::{
        AllocateType,
        MemoryMap,
        MemoryType,
        OpenProtocolAttributes,
        Pool,
        Protocol,
        SearchType,
    },
    protocols::{
        FileAttributes,
        FileInfo,
        FileMode,
        FileSystemInfo,
        SimpleFileSystem,
    },
    SystemTable,
    types::{
        Handle,
        Status,
    },
};
use xmas_elf::{
    ElfFile,
    header,
    program::{
        ProgramHeader,
        ProgramHeader64,
        Type,
    },
};


/// Print text to the console
macro_rules! efi_print {
    ($system_table:expr, $($arg:tt)*) => ({
        use core::fmt::Write;
        (&*($system_table).con_out)
            .write_fmt(format_args!($($arg)*))
            .expect("could not write to console");
    });
}


/// Print a line of text to the console
macro_rules! efi_println {
    ($system_table:expr, $fmt:expr) =>
        (efi_print!($system_table, concat!($fmt, "\r\n")));
    ($system_table:expr, $fmt:expr, $($arg:tt)*) =>
        (efi_print!($system_table, concat!($fmt, "\r\n"), $($arg)*));
}


/// Reads the specified file into memory
fn read_file<'a>(
    volume_label: &str,
    file_name: &str,
    image_handle: Handle,
    system_table: &'a SystemTable
) -> Result<Pool<'a, [u8]>, Status> {

    // Open the specified volume
    let vol_root = system_table.boot_services
        // Get the list of handles to available file systems
        .locate_handle(SearchType::ByProtocol, Some(SimpleFileSystem::guid()), None)?
        .iter()
        // Open each handle and get the root node
        .filter_map(|handle| {
            system_table.boot_services
                .open_protocol::<SimpleFileSystem>(
                    *handle,
                    image_handle,
                    0,
                    OpenProtocolAttributes::BY_HANDLE_PROTOCOL
                )
                .and_then(|vol| vol.open_volume())
                // If there was some issue opening a volume, just move on to the next one
                .ok()
        })
        // Keep only the volume with the specified label
        .find(|root| {
            root.get_info::<FileSystemInfo>(&*system_table.boot_services)
                .and_then(|info| info.volume_label(&*system_table.boot_services))
                .map(|label| label == volume_label)
                .unwrap_or(false)
        })
        .ok_or(Status::NotFound)?;

    // Open the specified file
    let path = boot_services::str_to_utf16(file_name, &system_table.boot_services)?;
    let file = vol_root.open(&path, FileMode::READ, FileAttributes::empty())?;

    // Allocate a suitably-sized buffer from pool memory
    let file_size = file
        .get_info::<FileInfo>(&*system_table.boot_services)?
        .file_size as usize;
    let mut file_buf = system_table.boot_services.allocate_slice::<u8>(file_size)?;

    // Read the entire file
    let _ = file.read(&mut file_buf)?;

    Ok(file_buf)
}


/// Loads the given ELF file and returns a pointer to its entry point
fn load_elf(
    elf_file: &ElfFile,
    system_table: &SystemTable
) -> Result<fn(&SystemTable, &MemoryMap) -> !, Status> {

    for header in elf_file.program_iter() {
        match header {
            ProgramHeader::Ph32(_) => return Err(Status::Unsupported),
            ProgramHeader::Ph64(header) => load_section(header, elf_file, system_table)?,
        }
    }

    efi_println!(system_table, "Entry point: {:x}", elf_file.header.pt2.entry_point());
    unsafe {
        Ok(mem::transmute(elf_file.header.pt2.entry_point()))
    }
}


/// Loads the given program segment into memory
fn load_section(
    header: &ProgramHeader64,
    elf_file: &ElfFile,
    system_table: &SystemTable
) -> Result<(), Status> {

    // Skip any section that's not loadable
    match header.get_type() {
        Err(err) => {
            efi_println!(system_table, "Failed to read section type: {}", err);
            return Err(Status::InvalidParameter);
        },
        Ok(Type::Load) => {},
        Ok(_) => return Ok(()),
    }

    // header.virtual_addr might not be on a 4K page boundary
    let mut destination: *mut u8 = (header.virtual_addr & !0x0fff) as *mut u8;

    // Don't just allocate enough room for header.mem_size, also account for any padding that may be
    // present between destination and header.virtual_addr
    let num_pages: usize = {
        let padding = header.virtual_addr & 0x0fff;
        let total_bytes = header.mem_size + padding;
        (1 + (total_bytes >> 12)) as usize
    };

    // Do the allocation and sanity-check that the firmware respected our requested address
    system_table.boot_services.allocate_pages(
        AllocateType::AllocateAddress,
        MemoryType::LoaderCode,
        num_pages,
        &mut destination
    )?;
    assert!(destination as u64 == header.virtual_addr & !0x0fff);

    // Zero out the allocated pages
    unsafe {
        system_table.boot_services.set_mem(destination, num_pages * 4096, 0);
    }

    // Copy any program bits to their destination
    let dst_buf = unsafe {
        slice::from_raw_parts_mut(header.virtual_addr as *mut u8, header.mem_size as usize)
    };
    dst_buf.copy_from_slice(header.raw_data(elf_file));

    Ok(())
}


#[no_mangle]
pub extern fn efi_main(image_handle: Handle, system_table: &SystemTable) -> ! {

    // Give debugger time to attach
    // loop { }

    // Store a reference to the system table to enable panic_fmt
    // This is safe at least until exit_boot_services is called
    unsafe {
        SYSTEM_TABLE = system_table;
    }

    efi_println!(system_table, "Reading kernel from EFISys");
    let res = read_file("EFISys", "EFI\\RustOS\\Kernel", image_handle, system_table);
    let mut file_buf = match res {
        Ok(buf) => buf,
        Err(err) => {
            efi_println!(system_table, "Failed to read kernel: {:?}", err);
            loop { }
        },
    };

    efi_println!(system_table, "Parsing ELF image and performing sanity check");
    let elf = match ElfFile::new(&mut file_buf) {
        Ok(elf) => elf,
        Err(err) => {
            efi_println!(system_table, "Failed to parse ELF image: {}", err);
            loop { }
        },
    };
    if let Err(err) = header::sanity_check(&elf) {
        efi_println!(system_table, "ELF sanity check failed: {}", err);
        loop { }
    }

    efi_println!(system_table, "Loading kernel into memory");
    let entry = match load_elf(&elf, system_table) {
        Ok(entry) => entry,
        Err(err) => {
            efi_println!(system_table, "Failed to load kernel: {:?}", err);
            loop { }
        },
    };

    efi_println!(system_table, "Retrieving memory map");
    // Evidently invoking con_out triggers memory allocations, so no more efi_println! after this
    let mem_map = match system_table.boot_services.get_memory_map() {
        Ok(map) => map,
        Err(err) => {
            efi_println!(system_table, "Failed to retrieve memory map: {:?}", err);
            loop { }
        },
    };

    if let Err(err) = system_table.boot_services.exit_boot_services(image_handle, mem_map.key) {
        efi_println!(system_table, "Failed to exit boot services: {:?}", err);
        loop { }
    }

    entry(system_table, &mem_map);
}


static mut SYSTEM_TABLE: *const SystemTable = 0 as *const _;


#[allow(private_no_mangle_fns)]
#[lang = "panic_fmt"]
#[no_mangle]
fn panic_fmt(args: &fmt::Arguments, file: &str, line: u32, col: u32) -> ! {

    let system_table = unsafe { SYSTEM_TABLE.as_ref().unwrap() };
    efi_println!(system_table, "Panic at {}:{}:{}", file, line, col);
    efi_println!(system_table, "{}", args);

    loop { }
}


#[allow(private_no_mangle_fns)]
#[lang = "eh_personality"]
#[no_mangle]
fn eh_personality() {

    loop { }
}
