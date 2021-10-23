#![feature(allocator_api)]
#![no_main]

use std::alloc::Allocator;
use std::alloc::Layout;
use std::ptr::NonNull;

use std::fs::File;
use std::io::SeekFrom;
use std::io::{Read, Seek, Write};
use std::os::unix::io::FromRawFd;

use libc;

use libshim;

#[link(name = "c")]
extern "C" {
    pub fn malloc(size: usize) -> *mut u8;
    pub fn free(ptr: *mut u8);
}

#[derive(Debug, Copy, Clone)]
struct StephenAllocator {}

impl libshim::Allocator for StephenAllocator {}

unsafe impl Allocator for StephenAllocator {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, std::alloc::AllocError> {
        // It's not entirely clear what to do about the alignment. I assume we
        // can just assume malloc does the right thing?

        let size = layout.size();
        let ptr: *mut u8 = unsafe { malloc(size) };

        if ptr.is_null() {
            Err(std::alloc::AllocError)
        } else {
            NonNull::new(std::ptr::slice_from_raw_parts_mut(ptr, size))
                .ok_or(std::alloc::AllocError)
        }
    }
    unsafe fn deallocate(&self, ptr: NonNull<u8>, _: Layout) {
        free(ptr.as_ptr())
    }
}

fn stdout() -> File {
    unsafe { File::from_raw_fd(1) }
}

struct FilePrinter {
    f: File,
}

impl libshim::Printer for FilePrinter {
    fn print(&mut self, text: &[u8]) {
        self.f.write(text).unwrap();
    }
}

#[no_mangle]
pub fn main(argc: i32, _argv: *const *const i8) -> Result<(), std::alloc::AllocError> {
    let mut stdout = stdout();

    // TODO: implement a REPL
    if argc != 2 {
        stdout
            .write(b"Expected a single script as an argument\n")
            .unwrap();
        return Ok(());
    }

    let allocator = StephenAllocator {};
    let script_name = unsafe { *_argv.offset(1) };

    // TODO: handle error codes
    // We open this ourselves since there's no way to open a file from a path
    // without using the global allocator...
    let fd = unsafe { libc::open(script_name, libc::O_RDONLY) };
    if fd == -1 {
        stdout.write(b"Error while opening script\n").unwrap();
        return Ok(());
    }
    let mut file = unsafe { File::from_raw_fd(fd) };

    let file_length = file.seek(SeekFrom::End(0)).unwrap() as usize;
    file.seek(SeekFrom::Start(0)).unwrap();

    let buf_layout = Layout::array::<u8>(file_length).map_err(|_| std::alloc::AllocError)?;
    let buf: NonNull<[u8]> = allocator.allocate(buf_layout)?;

    let count = unsafe { file.read(&mut *buf.as_ptr()).unwrap() };
    // Lazy file reading
    // TODO: did we read the whole file?
    assert_eq!(count, file_length);

    let mut interpreter = libshim::Interpreter::new(allocator);

    let mut stdout_printer = FilePrinter { f: stdout };
    interpreter.set_print_fn(&mut stdout_printer);
    interpreter.interpret(unsafe { &(*buf.as_ptr()) }).unwrap();

    unsafe { allocator.deallocate(buf.cast(), buf_layout) };

    Ok(())
}
