//! Host queue ownership for the normal window presentation path.
//!
//! Requests contain handles, not borrowed Vulkan structs. Each request owns
//! one frame slot until its host calls have returned; the slot's GPU fence
//! independently protects the resources used by the submitted commands.
//!
//! The worker touches the graphics queue and the swapchain, so the context
//! drains it before any other host access to those objects: the queue must be
//! externally synchronized, and `vkAcquireNextImageKHR` must not race the
//! worker's `vkQueuePresentKHR` on the same swapchain (see
//! `VulkanContext::begin_frame`).

use ash::{extensions::khr::Swapchain, vk, Device};
use std::{
    sync::mpsc::{self, Receiver, SyncSender},
    thread::{self, JoinHandle},
};

use super::MAX_FRAMES_IN_FLIGHT;

type PresentResult = Result<bool, vk::Result>;

pub(super) struct FrameSubmission {
    pub slot: usize,
    pub command_buffer: vk::CommandBuffer,
    pub wait: Option<vk::Semaphore>,
    pub signal: vk::Semaphore,
    pub fence: vk::Fence,
    pub swapchain: vk::SwapchainKHR,
    pub image_index: u32,
}

impl FrameSubmission {
    /// Both calls access the same queue and swapchain. Callers either run them
    /// on the queue worker or drain that worker before accessing the queue or
    /// the swapchain themselves.
    pub fn execute(
        &self,
        device: &Device,
        swapchain: &Swapchain,
        queue: vk::Queue,
    ) -> PresentResult {
        let command_buffers = [self.command_buffer];
        let wait_semaphores = [self.wait.unwrap_or_default()];
        let wait_stages = [vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT];
        let wait_count = usize::from(self.wait.is_some());
        let signal_semaphores = [self.signal];
        let submit = vk::SubmitInfo::builder()
            .command_buffers(&command_buffers)
            .wait_semaphores(&wait_semaphores[..wait_count])
            .wait_dst_stage_mask(&wait_stages[..wait_count])
            .signal_semaphores(&signal_semaphores);
        let swapchains = [self.swapchain];
        let image_indices = [self.image_index];
        let present = vk::PresentInfoKHR::builder()
            .wait_semaphores(&signal_semaphores)
            .swapchains(&swapchains)
            .image_indices(&image_indices);
        unsafe {
            device.queue_submit(queue, &[submit.build()], self.fence)?;
            match swapchain.queue_present(queue, &present) {
                Ok(suboptimal) => Ok(suboptimal),
                Err(vk::Result::ERROR_OUT_OF_DATE_KHR | vk::Result::SUBOPTIMAL_KHR) => Ok(true),
                Err(error) => Err(error),
            }
        }
    }
}

pub(super) struct SubmissionWorker {
    requests: Option<SyncSender<FrameSubmission>>,
    completed: Receiver<(usize, PresentResult)>,
    pending: [bool; MAX_FRAMES_IN_FLIGHT],
    thread: Option<JoinHandle<()>>,
}

impl SubmissionWorker {
    pub fn new(device: &Device, swapchain: &Swapchain, queue: vk::Queue) -> Option<Self> {
        let enabled = std::env::var("PRPR_VULKAN_SUBMIT_THREAD")
            .map(|value| {
                !matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "0" | "false" | "off"
                )
            })
            .unwrap_or(true);
        if !enabled {
            return None;
        }
        let device = device.clone();
        let swapchain = swapchain.clone();
        Some(Self::spawn(move |frame| {
            frame.execute(&device, &swapchain, queue)
        }))
    }

    fn spawn(mut execute: impl FnMut(FrameSubmission) -> PresentResult + Send + 'static) -> Self {
        let (requests, receiver) = mpsc::sync_channel::<FrameSubmission>(MAX_FRAMES_IN_FLIGHT);
        let (completed, completion_receiver) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("vulkan-submit".into())
            .spawn(move || {
                while let Ok(frame) = receiver.recv() {
                    let slot = frame.slot;
                    let result = execute(frame);
                    // The context may be unwinding; shutdown still consumes all
                    // queued requests before releasing the device references.
                    let _ = completed.send((slot, result));
                }
            })
            .expect("Failed to create the Vulkan submission thread");
        Self {
            requests: Some(requests),
            completed: completion_receiver,
            pending: [false; MAX_FRAMES_IN_FLIGHT],
            thread: Some(thread),
        }
    }

    pub fn submit(&mut self, frame: FrameSubmission) {
        let slot = frame.slot;
        assert!(
            !self.pending[slot],
            "frame slot still owned by the submission thread"
        );
        self.requests
            .as_ref()
            .unwrap()
            .send(frame)
            .expect("Vulkan submission thread stopped");
        self.pending[slot] = true;
    }

    fn complete(&mut self, (slot, result): (usize, PresentResult)) -> PresentResult {
        self.pending[slot] = false;
        result
    }

    pub fn wait_slot(&mut self, slot: usize) -> PresentResult {
        let mut recreate = false;
        while self.pending[slot] {
            let completion = self
                .completed
                .recv()
                .expect("Vulkan submission thread stopped");
            recreate |= self.complete(completion)?;
        }
        Ok(recreate)
    }

    /// Transfer exclusive queue access back to the calling thread. This only
    /// waits for host calls; the caller must also wait on the GPU before freeing
    /// resources or resetting commands that may still be in flight.
    pub fn drain(&mut self) -> PresentResult {
        let mut recreate = false;
        for slot in 0..MAX_FRAMES_IN_FLIGHT {
            recreate |= self.wait_slot(slot)?;
        }
        Ok(recreate)
    }
}

impl Drop for SubmissionWorker {
    fn drop(&mut self) {
        // Closing the request channel wakes an idle worker and lets an active
        // worker finish before the Vulkan context destroys its device.
        self.requests.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    fn frame(slot: usize) -> FrameSubmission {
        FrameSubmission {
            slot,
            command_buffer: vk::CommandBuffer::null(),
            wait: None,
            signal: vk::Semaphore::null(),
            fence: vk::Fence::null(),
            swapchain: vk::SwapchainKHR::null(),
            image_index: 0,
        }
    }

    #[test]
    fn submit_returns_before_host_work_finishes() {
        let (release, ready) = mpsc::channel();
        let mut worker = SubmissionWorker::spawn(move |_| {
            ready.recv().unwrap();
            Ok(false)
        });
        worker.submit(frame(0));
        assert!(worker.pending[0]);
        release.send(()).unwrap();
        assert_eq!(worker.wait_slot(0), Ok(false));
        assert!(!worker.pending[0]);
    }

    #[test]
    fn waiting_for_a_later_slot_preserves_earlier_present_results() {
        let mut worker = SubmissionWorker::spawn(|frame| Ok(frame.slot == 0));
        worker.submit(frame(0));
        worker.submit(frame(1));
        assert_eq!(worker.wait_slot(1), Ok(true));
        assert_eq!(worker.pending, [false; MAX_FRAMES_IN_FLIGHT]);
    }

    #[test]
    fn submission_failure_is_returned_before_waiting_on_a_gpu_fence() {
        let mut worker = SubmissionWorker::spawn(|_| Err(vk::Result::ERROR_DEVICE_LOST));
        worker.submit(frame(0));
        assert_eq!(worker.wait_slot(0), Err(vk::Result::ERROR_DEVICE_LOST));
        assert!(!worker.pending[0]);
    }

    #[test]
    fn shutdown_finishes_queued_host_work() {
        let completed = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&completed);
        let mut worker = SubmissionWorker::spawn(move |_| {
            count.fetch_add(1, Ordering::Relaxed);
            Ok(false)
        });
        worker.submit(frame(0));
        worker.submit(frame(1));
        drop(worker);
        assert_eq!(completed.load(Ordering::Relaxed), 2);
    }
}
