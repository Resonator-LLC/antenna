use criterion::{black_box, criterion_group, criterion_main, Criterion};
use std::thread;

use antenna::channel::InternalChannel;

fn bench_ring_buffer_push_pop(c: &mut Criterion) {
    let ch = InternalChannel::new(65536).unwrap();
    let writer = ch.writer();
    let reader = ch.reader();
    let msg = "[] a carrier:TextMessage ; carrier:text \"hello\" .";

    c.bench_function("ring_push_pop", |b| {
        b.iter(|| {
            writer.send(black_box(msg));
            black_box(reader.recv());
        })
    });
}

fn bench_ring_buffer_throughput(c: &mut Criterion) {
    let ch = InternalChannel::new(65536).unwrap();
    let writer = ch.writer();
    let reader = ch.reader();
    let msg = "[] a carrier:TextMessage ; carrier:text \"hello\" .";

    c.bench_function("ring_100_messages", |b| {
        b.iter(|| {
            for _ in 0..100 {
                writer.send(black_box(msg));
            }
            for _ in 0..100 {
                black_box(reader.recv());
            }
        })
    });
}

fn bench_cross_thread_channel(c: &mut Criterion) {
    c.bench_function("cross_thread_1000_msgs", |b| {
        b.iter(|| {
            let ch = InternalChannel::new(65536).unwrap();
            let writer = ch.writer();
            let reader = ch.reader();
            let msg = "[] a carrier:TextMessage ; carrier:text \"hello\" .";

            let sender = thread::spawn(move || {
                for _ in 0..1000 {
                    writer.send(msg);
                }
            });

            let mut count = 0;
            while count < 1000 {
                if reader.recv().is_some() {
                    count += 1;
                } else {
                    thread::yield_now();
                }
            }

            sender.join().unwrap();
            black_box(count);
        })
    });
}

criterion_group!(benches, bench_ring_buffer_push_pop, bench_ring_buffer_throughput, bench_cross_thread_channel);
criterion_main!(benches);
