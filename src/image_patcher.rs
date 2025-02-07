//  Copyright 2020 Two Sigma Investments, LP.
//
//  Licensed under the Apache License, Version 2.0 (the "License");
//  you may not use this file except in compliance with the License.
//  You may obtain a copy of the License at
//
//      http://www.apache.org/licenses/LICENSE-2.0
//
//  Unless required by applicable law or agreed to in writing, software
//  distributed under the License is distributed on an "AS IS" BASIS,
//  WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
//  See the License for the specific language governing permissions and
//  limitations under the License.

use anyhow::{Context, Result};
use bytes::Buf;

use std::{
    collections::HashMap,
    io::{Read, Write},
    mem::size_of,
};

use crate::{
    image_store::{self, ImageStore},
    util::{pb_read_next, read_bytes_next, pb_write},
    criu,
};

// These magic consts are defined in the CRIU project in criu/include/magic.h
const IMG_COMMON_MAGIC: u32 = 0x54564319;
const FILES_MAGIC: u32 = 0x56303138;

// From #include <netinet/tcp.h>
const TCP_LISTEN: u32 = 10;

fn read_criu_img_header(reader: &mut impl Read, expected_header_magic: u32) -> Result<()>
{
    let mut header = read_bytes_next(reader, 2*size_of::<u32>())?
        .ok_or_else(|| anyhow!("Failed to read the CRIU image header"))?;

    // We could add more specific error message, but I don't think it's
    // necessary given that this error should not happen unless we are getting
    // corrupted data.
    ensure!(header.get_u32_le() == IMG_COMMON_MAGIC &&
            header.get_u32_le() == expected_header_magic,
            "The CRIU image header is corrupted");

    Ok(())
}

fn write_criu_img_header(writer: &mut impl Write, header_magic: u32) -> Result<()>
{
    writer.write_all(&IMG_COMMON_MAGIC.to_le_bytes())?;
    writer.write_all(&header_magic.to_le_bytes())?;
    Ok(())
}

fn patch_tcp_listen_remaps(
    img_store: &mut image_store::mem::Store,
    tcp_listen_remaps: Vec<(u16, u16)>,
) -> Result<()>
{
    if tcp_listen_remaps.is_empty() {
        return Ok(());
    }

    let mut tcp_listen_remaps: HashMap<u16, u16> = tcp_listen_remaps.into_iter().collect();

    // remove() corresponds to HashMap::remove() in the memory store.
    let old_files = img_store.remove("files.img")
        .ok_or_else(|| anyhow!("files.img is missing from the image"))?;
    let mut old_files = old_files.reader();
    read_criu_img_header(&mut old_files, FILES_MAGIC)?;

    let mut new_files = img_store.create("files.img")?;
    write_criu_img_header(&mut new_files, FILES_MAGIC)?;

    // We take the original "files.img" file (`old_files`), we apply a few
    // transformations, and produce a new "files.img" file (`new_files`).
    // The file is a stream of protobuf items of type `criu::FileEntry`.
    // The following loop decodes each one of them in the `old_files`, modifies
    // the data structure if desired, and encodes it into `new_files`.
    //
    // It's a bit unfortunate that we decode and encode all entries in files.img
    // as all we do is patch a value within a few entries, but keep in mind that
    // the serialization of integers is of variable length with protobuf.
    //
    // An easy optimization we could do is to avoid re-encoding the file entry
    // when we don't do any modifications to it, we could copy the original
    // data.  Sadly, the library we use to decode the protobuf takes ownership
    // of the original data, so it's gone.

    // This vec is used to provide useful error messages
    let mut old_tcp_listen_ports = Vec::new();

    while let Some((mut file_entry, _)) = pb_read_next::<_,criu::FileEntry>(&mut old_files)? {
        if let Some(ref mut isk) = file_entry.isk {
            if isk.proto == libc::IPPROTO_TCP as u32 && isk.state == TCP_LISTEN {
                old_tcp_listen_ports.push(isk.src_port);
                if let Some(new_port) = tcp_listen_remaps.remove(&(isk.src_port as u16)) {
                    isk.src_port = new_port as u32;
                }
            }
        }
        pb_write(&mut new_files, &file_entry)?;
    }

    if !tcp_listen_remaps.is_empty() {
        let remap_ports_not_found = tcp_listen_remaps.keys().collect::<Vec<_>>();
        bail!("The following TCP listen ports were found in the checkpoint image: {:?}. \
               These requested port remaps could not be matched: {:?}",
              old_tcp_listen_ports, remap_ports_not_found);
    }

    img_store.insert("files.img", new_files);

    Ok(())
}

pub fn patch_img(
    img_store: &mut image_store::mem::Store,
    tcp_listen_remaps: Vec<(u16, u16)>,
) -> Result<()>
{
    patch_tcp_listen_remaps(img_store, tcp_listen_remaps)
        .context("Failed to remap TCP listen ports")?;
    Ok(())
}
