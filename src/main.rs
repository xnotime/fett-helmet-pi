#![forbid(unsafe_code)]

#![feature(exit_status_error)]

use std::{
    borrow::Cow,
    ffi::OsStr,
    fs::File,
    io::Write,
    ops::DerefMut,
    path::Path,
    process::Command,
};

use anyhow::Result;
use indicatif::ProgressBar;
use serialport::SerialPort;

const MCU_SERIAL_PORT: &'static str = "/dev/ttyUSB0";

const MAP_IMAGE_FILENAME: &'static str = "_map.png";

const INVERT_IMAGE: bool = false;

fn main() -> Result<()> {
    println!("[main] Establishing connection with {}...", MCU_SERIAL_PORT);
    let mut mcu = HelmetMcu::new(MCU_SERIAL_PORT)?;
    println!("[main] Loading map image...");
    load_map("59.438484,24.742595")?;
    println!("[main] Sending map image...");
    mcu.send_map()?;
    Ok(())
}

fn load_map(coords: impl AsRef<OsStr>) -> Result<()> {
    Command::new("./loadmap.sh")
        .arg(coords)
        .spawn()?
        .wait()?
        .exit_ok()?;
    Ok(())
}

struct HelmetMcu<S: DerefMut<Target = T>, T: Write + ?Sized> {
    serial: S,
    dims: (usize, usize),
}

const RESET_SEQ: [u8; 11] = [b'#'; 11];

impl HelmetMcu<Box<dyn SerialPort>, dyn SerialPort> {
    fn new<'a>(serial_port_path: impl Into<Cow<'a, str>>) -> Result<Self> {
        Ok(
            Self {
                serial: serialport::new(
                    serial_port_path, 115200,
                ).open()?,
                dims: (64, 64),
            }
        )
    }
}

impl<S: DerefMut<Target = T>, T: Write + ?Sized> HelmetMcu<S, T> {
    fn send_map(&mut self) -> Result<()> {
        self.send_png(MAP_IMAGE_FILENAME)
    }

    fn send_png(&mut self, filename: impl AsRef<Path>) -> Result<()> {
        let file = File::open(filename)?;
        let data = read_png(file)?;
        self.send_rotated(data)?;
        Ok(())
    }

    fn send_rotated(&mut self, data: Vec<u8>) -> Result<()> {
        self.send_raw(Rot90::new(data, self.dims))
    }

    fn send_raw(
        &mut self,
        data: impl Iterator<Item = u8>,
    ) -> Result<()> {
        println!("[send_raw] Sending reset sequence...");
        self.serial.write_all(&RESET_SEQ)?;
        self.serial.flush()?;
        let mut index_within_row = -1;
        let mut byte = 0x0_u8;
        let mut index_within_byte = 0;
        println!("[send_raw] Sending pixel data...");
        let prog = ProgressBar::new(64 * 9);
        for i in data {
            if (i > (u8::MAX / 2)) ^ INVERT_IMAGE {
                byte |= 1 << index_within_byte;
            }
            index_within_byte += 1;
            if index_within_byte >= 8 {
                index_within_byte = 0;
                self.serial.write_all(&[byte])?;
                prog.inc(1);
                byte = 0x0_u8;
                index_within_row += 1;
                if index_within_row >= 8 {
                    index_within_row = -1;
                    self.serial.flush()?;
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            }
            if index_within_row == -1 {
                self.serial.write_all(&[0x0])?;
                prog.inc(1);
                index_within_row += 1;
                continue;
            }
        }
        self.serial.flush()?;
        prog.finish();
        println!("[send_raw] All data sent and flushed.");
        Ok(())
    }
}

fn read_png(file: File) -> Result<Vec<u8>> {
    let mut reader = png::Decoder::new(file).read_info()?;
    let mut buf = vec![0; reader.output_buffer_size()];
    reader.next_frame(&mut buf)?;
    Ok(buf)
}

struct Rot90<T: Copy> {
    orig: Vec<T>,
    w: usize,
    h: usize,
    x: usize,
    y: usize,
}

impl<T: Copy> Rot90<T> {
    fn new(orig: Vec<T>, dims: (usize, usize)) -> Self {
        let (w, h) = dims;
        assert!(orig.len() == (w * h));
        Self {
            orig, w, h,
            x: 0, y: 0,
        }
    }
}

impl<T: Copy> Rot90<T> {
    fn at_pre(&self, xt: usize, yt: usize) -> Option<<Self as Iterator>::Item> {
        let index = (yt * self.w) + xt;
        if index >= self.orig.len() {
            None
        } else {
            Some(self.orig[(yt * self.w) + xt])
        }
    }

    fn internal_peek(&self) -> Option<<Self as Iterator>::Item> {
        let xt = self.y;
        let yt = self.w - self.x - 1;
        self.at_pre(xt, yt)
    }
}

impl<T: Copy> Iterator for Rot90<T> {
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        let ret = self.internal_peek();
        if ret.is_some() {
            self.x += 1;
            if self.x >= self.h {
                self.x = 0;
                self.y += 1;
            }
        }
        ret
    }
} 

