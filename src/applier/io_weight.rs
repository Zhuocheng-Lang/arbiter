use crate::platform::linux::Strategy;
use crate::rules::IoClass;

pub(super) fn effective_cgroup_weight(
    strategy: Strategy,
    configured_weight: Option<u64>,
) -> Option<u64> {
    if matches!(strategy, Strategy::NiceAndWeight) {
        configured_weight
    } else {
        None
    }
}

pub(super) fn ionice_to_io_weight(ioclass: Option<IoClass>, level: Option<u8>) -> Option<u16> {
    let class = ioclass?;
    let prio = level.unwrap_or(4).clamp(0, 7) as u16;

    let weight = match class {
        IoClass::RealTime => 800 + ((7 - prio) * 200) / 7,
        IoClass::BestEffort => 100 + ((7 - prio) * 700) / 7,
        IoClass::Idle => 1 + ((7 - prio) * 49) / 7,
        IoClass::None => return None,
    };

    Some(weight)
}

#[cfg(test)]
mod tests {
    use super::{effective_cgroup_weight, ionice_to_io_weight};
    use crate::platform::linux::Strategy;
    use crate::rules::IoClass;

    #[test]
    fn only_nice_and_weight_strategy_writes_weight() {
        assert_eq!(
            effective_cgroup_weight(Strategy::NiceAndWeight, Some(900)),
            Some(900)
        );
        assert_eq!(
            effective_cgroup_weight(Strategy::BasicHints, Some(900)),
            None
        );
        assert_eq!(
            effective_cgroup_weight(Strategy::LayeredJson, Some(900)),
            None
        );
    }

    #[test]
    fn ionice_maps_to_expected_io_weight_ranges() {
        assert_eq!(ionice_to_io_weight(Some(IoClass::None), Some(0)), None);
        assert_eq!(
            ionice_to_io_weight(Some(IoClass::RealTime), Some(0)),
            Some(1000)
        );
        assert_eq!(
            ionice_to_io_weight(Some(IoClass::RealTime), Some(7)),
            Some(800)
        );
        assert_eq!(
            ionice_to_io_weight(Some(IoClass::BestEffort), Some(0)),
            Some(800)
        );
        assert_eq!(
            ionice_to_io_weight(Some(IoClass::BestEffort), Some(7)),
            Some(100)
        );
        assert_eq!(ionice_to_io_weight(Some(IoClass::Idle), Some(0)), Some(50));
        assert_eq!(ionice_to_io_weight(Some(IoClass::Idle), Some(7)), Some(1));
    }
}
