use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    thread,
};

const TARGET_BUCKETS: usize = 128;

#[derive(Debug)]
pub enum EntropyMessage {
    Progress(usize),
    Done(Vec<f64>),
}

pub struct EntropyWorker {
    pub receiver: Receiver<EntropyMessage>,
    cancel: Arc<AtomicBool>,
}

impl EntropyWorker {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

pub fn spawn(bytes: Arc<Vec<u8>>) -> EntropyWorker {
    let (sender, receiver) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let worker_cancel = Arc::clone(&cancel);
    thread::spawn(move || {
        let profile = calculate_entropy(&bytes, &sender, &worker_cancel);
        if !worker_cancel.load(Ordering::Relaxed) {
            let _ = sender.send(EntropyMessage::Done(profile));
        }
    });
    EntropyWorker { receiver, cancel }
}

pub fn calculate(bytes: &[u8]) -> Vec<f64> {
    let (sender, _receiver) = mpsc::channel();
    calculate_entropy(bytes, &sender, &AtomicBool::new(false))
}

fn calculate_entropy(
    bytes: &[u8],
    sender: &mpsc::Sender<EntropyMessage>,
    cancel: &AtomicBool,
) -> Vec<f64> {
    if bytes.is_empty() {
        return vec![0.0];
    }
    let window = bytes.len().div_ceil(TARGET_BUCKETS).max(256);
    let mut scanned = 0;
    let mut profile = Vec::with_capacity(bytes.len().div_ceil(window));
    for chunk in bytes.chunks(window) {
        if cancel.load(Ordering::Relaxed) {
            return Vec::new();
        }
        let mut counts = [0usize; 256];
        for byte in chunk {
            counts[usize::from(*byte)] += 1;
        }
        profile.push(
            counts
                .into_iter()
                .filter(|count| *count > 0)
                .map(|count| {
                    let probability = count as f64 / chunk.len() as f64;
                    -probability * probability.log2()
                })
                .sum(),
        );
        scanned += chunk.len();
        if sender.send(EntropyMessage::Progress(scanned)).is_err() {
            return Vec::new();
        }
    }
    profile
}

#[cfg(test)]
mod tests {
    use super::calculate;

    #[test]
    fn calculates_entropy_for_uniform_and_varied_data() {
        assert!(
            calculate(&[0; 1024])
                .iter()
                .all(|entropy| entropy.abs() < f64::EPSILON)
        );
        assert!(calculate(&(0..=255).collect::<Vec<_>>())[0] > 7.9);
    }
}
