use ash::vk;

pub(super) fn choose_present_mode(
    supported: &[vk::PresentModeKHR],
    swap_interval: Option<i32>,
) -> vk::PresentModeKHR {
    if swap_interval == Some(0) && supported.contains(&vk::PresentModeKHR::IMMEDIATE) {
        return vk::PresentModeKHR::IMMEDIATE;
    }
    // Keep the previous default and use MAILBOX when IMMEDIATE is unavailable.
    // Vulkan requires every surface to support FIFO.
    if supported.contains(&vk::PresentModeKHR::MAILBOX) {
        vk::PresentModeKHR::MAILBOX
    } else {
        vk::PresentModeKHR::FIFO
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vk::PresentModeKHR as Mode;

    #[test]
    fn zero_interval_prefers_immediate_regardless_of_driver_order() {
        for modes in [
            [Mode::FIFO, Mode::MAILBOX, Mode::IMMEDIATE],
            [Mode::IMMEDIATE, Mode::FIFO, Mode::MAILBOX],
            [Mode::MAILBOX, Mode::IMMEDIATE, Mode::FIFO],
        ] {
            assert_eq!(choose_present_mode(&modes, Some(0)), Mode::IMMEDIATE);
        }
    }

    #[test]
    fn zero_interval_falls_back_only_to_supported_modes() {
        assert_eq!(
            choose_present_mode(&[Mode::FIFO, Mode::MAILBOX], Some(0)),
            Mode::MAILBOX
        );
        assert_eq!(choose_present_mode(&[Mode::FIFO], Some(0)), Mode::FIFO);
        assert_eq!(
            choose_present_mode(&[Mode::FIFO, Mode::FIFO_RELAXED], Some(0)),
            Mode::FIFO
        );
    }

    #[test]
    fn other_intervals_preserve_previous_mailbox_then_fifo_preference() {
        for interval in [None, Some(-1), Some(1), Some(2)] {
            assert_eq!(
                choose_present_mode(&[Mode::FIFO, Mode::IMMEDIATE, Mode::MAILBOX], interval),
                Mode::MAILBOX
            );
            assert_eq!(
                choose_present_mode(&[Mode::FIFO, Mode::IMMEDIATE], interval),
                Mode::FIFO
            );
        }
    }
}
