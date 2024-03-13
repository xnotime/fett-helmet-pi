#![forbid(unsafe_code)]

#![feature(exit_status_error)]

use std::{
    borrow::Cow,
    convert::Infallible,
    ffi::OsStr,
    fs::File,
    io::Write,
    net::SocketAddr,
    ops::DerefMut,
    path::Path,
    process::Command,
    thread::{sleep, spawn},
    time::Instant,
};

use anyhow::Result;
use crossbeam_channel::{bounded, Sender, Receiver};
use indicatif::ProgressBar;
use lazy_static::lazy_static;
use png::Decoder as PngDec;
use serialport::SerialPort;
use warp::Filter;

const SERVER_ADDR: &'static str = "0.0.0.0:8080";

const MCU_SERIAL_PORT: &'static str = "/dev/ttyUSB0";

const MAP_IMAGE_FILENAME: &'static str = "_map.png";

const INVERT_IMAGE: bool = false;

type UpdateT = String;

#[tokio::main]
async fn main() -> Result<()> {
    // normal_mode().await
    touhou_mode().await
}

async fn touhou_mode() -> Result<()> {
    let mut mcu = HelmetMcu::new(MCU_SERIAL_PORT)?;
    let start = Instant::now();
    let mut last_frame_sent = -1;
    loop {
        let time = start.elapsed().as_millis() as i64;
        let frame = (time / 500) + 1;
        if frame != last_frame_sent {
            let filename = format!("BadApple64x64/frame_{frame:03?}.png");
            println!("{filename:?}");
            mcu.send_png_g(filename)?;
            last_frame_sent = frame;
        }
    }
}

async fn normal_mode() -> Result<()> {
    lazy_static! {
        static ref UP_CHAN: (Sender<UpdateT>, Receiver<UpdateT>) = bounded(0);
        static ref UP_TX: &'static Sender<UpdateT> = &UP_CHAN.0;
        static ref UP_RX: &'static Receiver<UpdateT> = &UP_CHAN.1;
    }
    println!("[main] Connecting to microcontroller...");
    let mut mcu = HelmetMcu::new(MCU_SERIAL_PORT)?;
    println!("[main] Spawning update handler thread...");
    spawn(move || {
        spawn(move || -> Result<Infallible> {
            println!("[update handler] Listening on rendevous channel...");
            for coords in *UP_RX {
                println!("[update handler] Loading map at {coords}...");
                load_map(coords)?;
                println!("[update handler] Sending map...");
                let start = Instant::now();
                mcu.send_map()?;
                let elapsed = start.elapsed().as_millis();
                println!("[update handler] Sent map in {elapsed:.2?}ms.")
            }
            unreachable!()
        }).join().unwrap().unwrap();
    });
    println!("[main] Setting up warp...");
    let html = warp::any().map(move || {
        println!("[warp filter] [GET] Serving index.html...");
        warp::reply::html(include_str!("index.html"))
    });
    let data = warp::path!("coords" / String)
        .then(|coords| async {
            println!("[warp filter] [POST /coords] Rendezvousing...");
            UP_TX.send(coords).unwrap();
            "ok"
        });
    let routes = warp::get().and(html)
        .or(warp::post().and(data));
    println!("[main] Serving via warp...");
    let socket_addr: SocketAddr = SERVER_ADDR.parse()?;
    warp::serve(routes).run(socket_addr).await;
    unreachable!()
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

const ROWS_BETWEEN_SLEEPS: u8 = 2;
const SLEEP_TIME_MILLIS: u64 = 17;

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

    fn send_png_1bit(&mut self, filename: impl AsRef<Path>) -> Result<()> {
        let file = File::open(filename)?;
        let data = read_png_1bit(file)?;
        self.send_rotated(data)?;
        Ok(())
    }

    fn send_png_g(&mut self, filename: impl AsRef<Path> + Clone) -> Result<()> {
        let file = File::open(filename.clone())?;
        let mut data = read_png(file)?;
        if data.len() != (64 * 64) {
            assert!(data.len() == (64 * 64 / 8));
            self.send_png_1bit(filename.clone())?;
        } else {
            self.send_rotated(data)?;
        }
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
        let mut rows_since_sleep = ROWS_BETWEEN_SLEEPS;
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
                    if rows_since_sleep >= ROWS_BETWEEN_SLEEPS {
                        sleep(std::time::Duration::from_millis(
                            SLEEP_TIME_MILLIS
                        ));
                        rows_since_sleep = 0;
                    } else {
                        rows_since_sleep += 1;
                    }
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

fn read_png_1bit(file: File) -> Result<Vec<u8>> {
    let mut reader = PngDec::new(file).read_info()?;
    let mut raw_buf = vec![0; reader.output_buffer_size()];
    let mut buf = vec![0u8; raw_buf.len() * 8];
    reader.next_frame(&mut raw_buf)?;
    let mut index = 0;
    for i in 0..(raw_buf.len()) {
        for j in 0..8 {
            if (raw_buf[i] & (1 << j)) > 0 {
                buf[(i * 8) + j] = 0xFF;
            }
        }
    }
    Ok(buf)
}

fn read_png(file: File) -> Result<Vec<u8>> {
    let mut reader = PngDec::new(file).read_info()?;
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

