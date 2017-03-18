// Copyright 2017 Google Inc. All rights reserved.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

extern crate coreaudio;
extern crate coremidi;
extern crate time;

extern crate synthesizer_io;

use coreaudio::audio_unit::{AudioUnit, IOType, SampleFormat};
use coreaudio::audio_unit::render_callback::{self, data};

use synthesizer_io::modules;

use synthesizer_io::worker::Worker;
use synthesizer_io::queue::Sender;
use synthesizer_io::graph::{Node, Message, SetParam, Note};
use synthesizer_io::module::N_SAMPLES_PER_CHUNK;

fn set_ctrl_const(value: u8, lo: f32, hi: f32, ix: usize, tx: &Sender<Message>, ts: u64) {
    let value = lo + value as f32 * (1.0/127.0) * (hi - lo);
    let param = SetParam {
        ix: ix,
        param_ix: 0,
        val: value,
        timestamp: ts,
    };
    tx.send(Message::SetParam(param));
}

fn send_note(ixs: Vec<usize>, midi_num: f32, velocity: f32, on: bool,
    tx: &Sender<Message>, ts: u64)
{
    let note = Note {
        ixs: ixs.into_boxed_slice(),
        midi_num: midi_num,
        velocity: velocity,
        on: on,
        timestamp: ts,
    };
    tx.send(Message::Note(note));
}

fn dispatch_midi(data: &[u8], tx: &Sender<Message>, ts: u64) {
    let mut i = 0;
    while i < data.len() {
        if data[i] == 0xb0 {
            let controller = data[i + 1];
            let value = data[i + 2];
            match controller {
                1 => set_ctrl_const(value, 0.0, 22_000f32.log2(), 3, tx, ts),
                2 => set_ctrl_const(value, 0.0, 0.995, 4, tx, ts),
                3 => set_ctrl_const(value, 0.0, 22_000f32.log2(), 5, tx, ts),
                _ => println!("don't have handler for controller {}", controller),
            }
            i += 3;
        } else if data[i] == 0x90 || data[i] == 0x80 {
            let midi_num = data[i + 1];
            let velocity = data[i + 2];
            let on = data[i] == 0x90 && velocity > 0;
            println!("{} {}", data[i + 1], data[i + 2]);
            send_note(vec![5], midi_num as f32, velocity as f32, on, tx, ts);
            i += 3;
        } else {
            break;
        }
    }
}

fn main() {
    let (mut worker, tx, rx) = Worker::create(1024);

    /*
    let module = Box::new(modules::ConstCtrl::new(440.0f32.log2()));
    worker.handle_node(Node::create(module, 1, [], []));
    let module = Box::new(modules::Sin::new(44_100.0));
    worker.handle_node(Node::create(module, 2, [], [(1, 0)]));
    let module = Box::new(modules::ConstCtrl::new(880.0f32.log2()));
    worker.handle_node(Node::create(module, 3, [], []));
    let module = Box::new(modules::Sin::new(44_100.0));
    worker.handle_node(Node::create(module, 4, [], [(3, 0)]));
    let module = Box::new(modules::Sum);
    worker.handle_node(Node::create(module, 0, [(2, 0), (4, 0)], []));
    */

    let module = Box::new(modules::Saw::new(44_100.0));
    worker.handle_node(Node::create(module, 1, [], [(5, 0)]));
    let module = Box::new(modules::SmoothCtrl::new(880.0f32.log2()));
    worker.handle_node(Node::create(module, 3, [], []));
    let module = Box::new(modules::SmoothCtrl::new(0.5));
    worker.handle_node(Node::create(module, 4, [], []));
    let module = Box::new(modules::NotePitch::new());
    worker.handle_node(Node::create(module, 5, [], []));
    let module = Box::new(modules::Biquad::new(44_100.0));
    worker.handle_node(Node::create(module, 0, [(1,0)], [(3, 0), (4, 0)]));

    let _audio_unit = run(worker).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1_000));

    let module = Box::new(modules::SmoothCtrl::new((440.0f32 * 1.5).log2()));
    let node = Node::create(module, 3, [], []);
    tx.send(Message::Node(node));
    let source_index = 0;
    if source_index < coremidi::Sources::count() {
        let source = coremidi::Source::from_index(source_index);
        println!("Listening for midi from {}", source.display_name().unwrap());
        let client = coremidi::Client::new("synthesizer-client").unwrap();
        let mut last_ts = 0;
        let mut last_val = 0;
        let callback = move |packet_list: &coremidi::PacketList| {
            for packet in packet_list.iter() {
                let data = packet.data();
                let delta_t = packet.timestamp() - last_ts;
                let speed = 1e9 * (data[2] as f64 - last_val as f64) / delta_t as f64;
                println!("{} {:3.3} {} {}", speed, delta_t as f64 * 1e-6, data[2],
                    time::precise_time_ns() - packet.timestamp());
                last_val = data[2];
                last_ts = packet.timestamp();
                dispatch_midi(&data, &tx, last_ts);
            }
        };
        let input_port = client.input_port("synthesizer-port", callback).unwrap();
        input_port.connect_source(&source).unwrap();

        println!("Press Enter to exit.");
        let mut line = String::new();
        ::std::io::stdin().read_line(&mut line).unwrap();
        input_port.disconnect_source(&source).unwrap();
    } else {
        println!("No midi available");
    }
}

fn run(mut worker: Worker) -> Result<AudioUnit, coreaudio::Error> {

    // Construct an Output audio unit that delivers audio to the default output device.
    let mut audio_unit = AudioUnit::new(IOType::DefaultOutput)?;

    let stream_format = audio_unit.stream_format()?;
    //println!("{:#?}", &stream_format);

    // We expect `f32` data.
    assert!(SampleFormat::F32 == stream_format.sample_format);

    type Args = render_callback::Args<data::NonInterleaved<f32>>;
    audio_unit.set_render_callback(move |args| {
        let Args { num_frames, mut data, .. }: Args = args;
        assert!(num_frames % N_SAMPLES_PER_CHUNK == 0);
        let mut i = 0;
        let mut timestamp = time::precise_time_ns();
        while i < num_frames {
            // should let the graph generate stereo
            let buf = worker.work(timestamp)[0].get();
            for j in 0..N_SAMPLES_PER_CHUNK {
                for channel in data.channels_mut() {
                    channel[i + j] = buf[j];
                }
            }
            timestamp += 1451247;  // 64 * 1e9 / 44_100
            i += N_SAMPLES_PER_CHUNK;
        }
        Ok(())
    })?;
    audio_unit.start()?;

    Ok(audio_unit)
}
