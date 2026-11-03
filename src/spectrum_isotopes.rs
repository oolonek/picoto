//! Candidate isotope annotations by m/z spacing; these are not chemical identifications.

use crate::{
    feature::{ISOTOPE_SPACING, isotope_tolerance},
    spectrum::Peak,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct IsotopeMatch {
    pub parent: usize,
    pub order: u8,
}

/// Annotate positive-intensity daughter peaks in m/z-sorted centroid data.
/// If assignments overlap, prefer the closest expected m/z, then the earlier
/// parent index, then the lower isotope order.
pub(crate) fn isotope_matches(peaks: &[Peak]) -> Vec<Option<IsotopeMatch>> {
    let mut matches: Vec<Option<IsotopeMatch>> = vec![None; peaks.len()];
    for (parent, mono) in peaks.iter().enumerate() {
        if !mono.mz.is_finite() || mono.mz <= 0.0 || !(mono.intensity > 0.0) {
            continue;
        }
        for order in 1..=3_u8 {
            let target = mono.mz + f64::from(order) * ISOTOPE_SPACING;
            if !target.is_finite() {
                continue;
            }
            let tolerance = isotope_tolerance(target);
            let first = peaks.partition_point(|peak| peak.mz < target - tolerance);
            let Some((offset, daughter)) = peaks[first..]
                .iter()
                .take_while(|peak| peak.mz <= target + tolerance)
                .enumerate()
                .min_by(|(_, left), (_, right)| {
                    (left.mz - target)
                        .abs()
                        .total_cmp(&(right.mz - target).abs())
                })
            else {
                continue;
            };
            let index = first + offset;
            if index <= parent
                || daughter.mz <= mono.mz
                || daughter.mz <= 0.0
                || !(daughter.intensity > 0.0)
            {
                continue;
            }
            let error = (daughter.mz - target).abs();
            let replace = match matches[index] {
                None => true,
                Some(previous) => {
                    let previous_target =
                        peaks[previous.parent].mz + f64::from(previous.order) * ISOTOPE_SPACING;
                    let previous_error = (daughter.mz - previous_target).abs();
                    error < previous_error
                        || (error == previous_error
                            && (parent < previous.parent
                                || (parent == previous.parent && order < previous.order)))
                }
            };
            if replace {
                matches[index] = Some(IsotopeMatch { parent, order });
            }
        }
    }
    matches
}

#[cfg(test)]
mod tests {
    use super::{IsotopeMatch, isotope_matches};
    use crate::spectrum::Peak;

    fn peak(mz: f64, intensity: f64) -> Peak {
        Peak { mz, intensity }
    }

    #[test]
    fn marks_each_isotope_order_and_retains_parent_index() {
        let peaks = [
            peak(100.0, 100.0),
            peak(101.003_355, 20.0),
            peak(102.006_710, 10.0),
            peak(103.010_065, 5.0),
        ];
        assert_eq!(
            isotope_matches(&peaks),
            vec![
                None,
                Some(IsotopeMatch {
                    parent: 0,
                    order: 1
                }),
                Some(IsotopeMatch {
                    parent: 0,
                    order: 2
                }),
                Some(IsotopeMatch {
                    parent: 0,
                    order: 3
                }),
            ]
        );
    }

    #[test]
    fn skips_missing_noisy_and_zero_intensity_peaks() {
        let peaks = [
            peak(100.0, 40.0),
            peak(101.003_355, 0.0),
            peak(102.002, 2.0),
            peak(102.006_710, 4.0),
            peak(103.020, 3.0),
            peak(200.0, 0.0),
            peak(201.003_355, 1.0),
        ];
        assert_eq!(
            isotope_matches(&peaks),
            vec![
                None,
                None,
                None,
                Some(IsotopeMatch {
                    parent: 0,
                    order: 2
                }),
                None,
                None,
                None,
            ]
        );
    }

    #[test]
    fn includes_tolerance_boundary_but_not_peaks_beyond_it() {
        let target = 101.003_355;
        let lower = [
            peak(100.0, 10.0),
            peak(target - 0.004_001, 3.0),
            peak(target - 0.004, 2.0),
        ];
        assert_eq!(
            isotope_matches(&lower),
            vec![
                None,
                None,
                Some(IsotopeMatch {
                    parent: 0,
                    order: 1
                })
            ]
        );
        let upper = [
            peak(100.0, 10.0),
            peak(target + 0.004, 2.0),
            peak(target + 0.004_001, 3.0),
        ];
        assert_eq!(
            isotope_matches(&upper),
            vec![
                None,
                Some(IsotopeMatch {
                    parent: 0,
                    order: 1
                }),
                None
            ]
        );
    }

    #[test]
    fn marks_only_the_nearest_daughter_per_parent_and_order() {
        let target = 101.003_355;
        let candidates = [
            peak(100.0, 10.0),
            peak(target - 0.003, 2.0),
            peak(target + 0.001, 3.0),
        ];
        assert_eq!(
            isotope_matches(&candidates),
            vec![
                None,
                None,
                Some(IsotopeMatch {
                    parent: 0,
                    order: 1
                })
            ]
        );
        let zero_nearest = [
            peak(100.0, 10.0),
            peak(target - 0.003, 2.0),
            peak(target, 0.0),
        ];
        assert_eq!(isotope_matches(&zero_nearest), vec![None, None, None]);
    }

    #[test]
    fn uses_ppm_tolerance_at_high_mz() {
        let peaks = [peak(1000.0, 10.0), peak(1001.013, 2.0), peak(1001.014, 2.0)];
        assert_eq!(
            isotope_matches(&peaks),
            vec![
                None,
                Some(IsotopeMatch {
                    parent: 0,
                    order: 1
                }),
                None
            ]
        );
    }

    #[test]
    fn overlapping_parents_use_closest_target_and_deterministic_ties() {
        let peaks = [
            peak(100.0, 10.0),
            peak(100.002, 10.0),
            peak(101.003_355, 3.0),
            peak(101.005_355, 3.0),
        ];
        assert_eq!(
            isotope_matches(&peaks),
            vec![
                None,
                None,
                Some(IsotopeMatch {
                    parent: 0,
                    order: 1
                }),
                Some(IsotopeMatch {
                    parent: 1,
                    order: 1
                }),
            ]
        );
    }

    #[test]
    fn identical_parent_candidates_prefer_first_peak() {
        let peaks = [peak(100.0, 20.0), peak(100.0, 10.0), peak(101.003_355, 2.0)];
        assert_eq!(
            isotope_matches(&peaks),
            vec![
                None,
                None,
                Some(IsotopeMatch {
                    parent: 0,
                    order: 1
                })
            ]
        );
    }

    #[test]
    fn does_not_annotate_nonpositive_mz_or_nonpositive_intensity() {
        let peaks = [
            peak(-1.003_355, 10.0),
            peak(0.0, 10.0),
            peak(1.003_355, -1.0),
            peak(2.006_710, 2.0),
        ];
        assert_eq!(isotope_matches(&peaks), vec![None; peaks.len()]);
    }
}
