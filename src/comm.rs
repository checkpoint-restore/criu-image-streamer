use comm::{
    do_comm_server::{DoCommServer},
    StreamReply,
    StreamRequest,
};
use crate::comm::do_comm_server::DoComm;
use image_streamer::{
    capture::capture,
    criu_connection::CriuListener,
    extract::serve,
    unix_pipe::UnixPipe,
};
use libc::dup;
use lz4_flex::frame::{FrameEncoder, FrameDecoder};
use nix::unistd::{close, pipe};
use std::{
    io::{self, Read, Write},
    os::unix::io::{FromRawFd, RawFd},
    path::Path,
    fs::{self, File},
    thread,
};
use tonic::{transport::Server, Request, Response, Status};

pub mod comm {
    tonic::include_proto!("comm");
}

#[derive(Debug, Default)]
pub struct Comm {}

#[tonic::async_trait]
impl DoComm for Comm {
    async fn do_capture(&self, request: Request<StreamRequest>) -> Result<Response<StreamReply>, Status> {
        
        let dir_path_string = request.into_inner().images_dir;
        let dir_path = Path::new(&dir_path_string);

        let (r_fd, w_fd): (RawFd, RawFd) = pipe().map_err(|err| {
            Status::internal(format!("Pipe creation failed: {}", err))
        })?;

        let dup_fd = unsafe { dup(w_fd) };
        close(w_fd).expect("Failed to close original write file descriptor");
        let shard_pipes: Vec<UnixPipe> = {
            vec![unsafe { fs::File::from_raw_fd(dup_fd) }]
        };

        let stats = capture(dir_path, shard_pipes);
        let stats_ref = stats.as_ref().unwrap();

        let output_file_path = dir_path.join("img.lz4");
        let output_file = File::create(output_file_path)?;
        let mut input_file = unsafe { File::from_raw_fd(r_fd) };
        let mut encoder = FrameEncoder::new(output_file);
        io::copy(&mut input_file, &mut encoder)?;
        let result = encoder.finish();

        let reply = comm::StreamReply {
            bytes_written: stats_ref.size,
            transfer_duration_millis: stats_ref.transfer_duration_millis,
            lz4_status: result.is_ok(),
        };

        Ok(Response::new(reply))
    }

    async fn do_serve(&self, request: Request<StreamRequest>) -> Result<Response<StreamReply>, Status> {
        
        let dir_path_string = request.into_inner().images_dir;
        let dir_path = Path::new(&dir_path_string);

        let ced_listener = CriuListener::bind_for_restore_ced(dir_path)
            .map_err(|e| Status::internal(format!("Failed to bind listener: {}", e)))?;
        let criu_listener = CriuListener::bind_for_restore(dir_path)
            .map_err(|e| Status::internal(format!("Failed to bind listener: {}", e)))?;

        let (r_fd, w_fd): (RawFd, RawFd) = pipe().map_err(|err| {
            Status::internal(format!("Pipe creation failed: {}", err))
        })?;

        let input_file_path = dir_path.join("img.lz4");
        let input_file = File::open(input_file_path)?;
        let mut output_file = unsafe { File::from_raw_fd(w_fd) };
        let mut decoder = FrameDecoder::new(input_file);
        let mut buf = Vec::new();
        decoder.read_to_end(&mut buf)?;
        thread::spawn(move || {
            let _ = output_file.write_all(&buf);
        });

        let dup_fd = unsafe { dup(r_fd) };
        close(r_fd).expect("Failed to close original read file descriptor");
        let shard_pipes = vec![unsafe { fs::File::from_raw_fd(dup_fd) }];

        thread::spawn(move || {
            let _ = serve(shard_pipes, ced_listener, criu_listener);
        });

        let reply = comm::StreamReply {
            bytes_written: 0,
            transfer_duration_millis: 0,
            lz4_status: true,
        };
        Ok(Response::new(reply))

    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr = "[::1]:6000".parse()?;

    let comm = Comm::default();

    Server::builder()
        .add_service(DoCommServer::new(comm))
        .serve(addr)
        .await?;

    Ok(())
}


