// Exact reconstruction of the legacy 49 linear samples at pixel-centred output.
// Only sigma <= 8 has a dense support small enough to reduce texture reads.
#[repr(C)]
#[derive(Clone, Copy)]
struct BackdropKernel {
    count: u32,
    padding: [u32; 3],
    taps: [[f32; 4]; 49],
}
impl BackdropKernel {
    fn new(sigma: f32, enabled: bool) -> Self {
        let mut result = Self { count: 0, padding: [0; 3], taps: [[0.; 4]; 49] };
        if !enabled || !sigma.is_finite() || sigma <= 0. {
            return result;
        }
        if sigma > 8. {
            let mut total = 0.0;
            for (index, i) in (-24..=24).enumerate() {
                let d = i as f32 * (sigma / 8.0).max(0.125);
                let w = (-0.5 * d * d / (sigma * sigma).max(0.001)).exp();
                result.taps[index] = [d, w, 0.0, 0.0];
                total += w;
            }
            for tap in &mut result.taps {
                tap[1] /= total;
            }
            result.count = 49;
            return result;
        }
        let mut weights = [0.0f32; 49];
        let mut total = 0.0;
        for i in -24..=24 {
            let d = i as f32 * (sigma / 8.0).max(0.125);
            let w = (-0.5 * d * d / (sigma * sigma).max(0.001)).exp();
            let lo = d.floor();
            let fraction = d - lo;
            let index = (lo as i32 + 24) as usize;
            weights[index] += w * (1.0 - fraction);
            if fraction > 0.0 {
                weights[index + 1] += w * fraction;
            }
            total += w;
        }
        // Pair adjacent texels using the texture unit's linear interpolation.
        // Clamp-to-edge commutes with this weighted reconstruction at crop edges.
        for pair in (0..49).step_by(2) {
            let a = weights[pair];
            let b = weights.get(pair + 1).copied().unwrap_or(0.0);
            let weight = a + b;
            if weight == 0.0 {
                continue;
            }
            result.taps[result.count as usize] = [pair as f32 - 24.0 + b / weight, weight / total, 0.0, 0.0];
            result.count += 1;
        }
        result
    }
    /// One pass of the half-resolution path (N2) at `sigma` in half-size texels.
    /// Dense Gaussian taps out to 3 sigma, cut at 48 texels (2.4 sigma at most,
    /// up to sigma 20) so they fold into at most 49 linear reads; wider blurs keep
    /// the sparse 49 positions of `new`. The horizontal pass reads the full-size
    /// target, where a linear read between two half texels is not their mix, so
    /// it folds only when the unfolded taps would not fit (sigma > 8).
    fn half(sigma: f32, horizontal: bool) -> Self {
        let mut result = Self { count: 0, padding: [0; 3], taps: [[0.; 4]; 49] };
        if !sigma.is_finite() || sigma <= 0. {
            return result;
        }
        let reach = (3. * sigma).ceil() as i32;
        if reach > 60 {
            return Self::new(sigma, true);
        }
        let reach = reach.min(48);
        let weights: Vec<f32> = (-reach..=reach).map(|i| (-0.5 * (i * i) as f32 / (sigma * sigma)).exp()).collect();
        let total: f32 = weights.iter().sum();
        if horizontal && weights.len() <= 49 {
            for (index, w) in weights.iter().enumerate() {
                result.taps[index] = [(index as i32 - reach) as f32, w / total, 0., 0.];
            }
            result.count = weights.len() as u32;
            return result;
        }
        for pair in (0..weights.len()).step_by(2) {
            let a = weights[pair];
            let b = weights.get(pair + 1).copied().unwrap_or(0.);
            result.taps[result.count as usize] = [(pair as i32 - reach) as f32 + b / (a + b), (a + b) / total, 0., 0.];
            result.count += 1;
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample(values: &[f32], x: f32) -> f32 {
        let lo = x.floor();
        let f = x - lo;
        let at = |i: f32| values[(i as i32).clamp(0, values.len() as i32 - 1) as usize];
        at(lo) * (1.0 - f) + at(lo + 1.0) * f
    }
    #[test]
    fn reconstructed_kernel_preserves_linear_samples_including_edges() {
        let values: Vec<_> = (0..97).map(|i| ((i * 13 + i * i * 7) % 97) as f32 / 97.0).collect();
        for sigma in (1..=800).map(|n| n as f32 / 100.).chain([8.01, 16., 40., 128.]) {
            let kernel = BackdropKernel::new(sigma, true);
            assert!(kernel.count > 0 && kernel.count <= (if sigma <= 8. { 25 } else { 49 }));
            for centre in [0., 1., 4., 48., 94., 96.] {
                let mut expected = 0.;
                let mut total = 0.;
                for i in -24..=24 {
                    let d = i as f32 * (sigma / 8.).max(0.125);
                    let w = (-0.5 * d * d / (sigma * sigma).max(0.001)).exp();
                    expected += sample(&values, centre + d) * w;
                    total += w;
                }
                let actual: f32 = kernel.taps[..kernel.count as usize].iter().map(|t| sample(&values, centre + t[0]) * t[1]).sum();
                assert!((actual - expected / total).abs() < 0.00001, "sigma={sigma} centre={centre}");
            }
        }
    }
    #[test]
    fn half_kernels_are_dense_gaussians_within_49_reads() {
        let values: Vec<_> = (0..241).map(|i| ((i * 29 + i * i * 11) % 241) as f32 / 241.0).collect();
        for sigma in [4.01f32, 6., 8., 12., 16., 18., 20.] {
            let reach = ((3. * sigma).ceil() as i32).min(48);
            let total: f32 = (-reach..=reach).map(|i| (-0.5 * (i * i) as f32 / (sigma * sigma)).exp()).sum();
            for horizontal in [false, true] {
                let kernel = BackdropKernel::half(sigma, horizontal);
                assert!(kernel.count > 0 && kernel.count <= 49, "sigma={sigma}");
                let sum: f32 = kernel.taps[..kernel.count as usize].iter().map(|t| t[1]).sum();
                assert!((sum - 1.).abs() < 1e-5);
                if horizontal && sigma <= 8. {
                    assert!(kernel.taps[..kernel.count as usize].iter().all(|t| t[0].fract() == 0.), "sigma={sigma}: block samples must not interpolate");
                }
                for centre in [60f32, 120., 180.] {
                    let expected: f32 = (-reach..=reach).map(|i| values[(centre as i32 + i) as usize] * (-0.5 * (i * i) as f32 / (sigma * sigma)).exp()).sum::<f32>() / total;
                    let actual: f32 = kernel.taps[..kernel.count as usize].iter().map(|t| sample(&values, centre + t[0]) * t[1]).sum();
                    assert!((actual - expected).abs() < 1e-5, "sigma={sigma} horizontal={horizontal}");
                }
            }
        }
        assert_eq!(BackdropKernel::half(40., false).count, 49);
        assert_eq!(BackdropKernel::half(40., false).taps[1][0], BackdropKernel::new(40., true).taps[1][0]);
    }
    #[test]
    fn dense_kernel_is_bounded_and_wide_blur_retains_original_positions() {
        assert_eq!(std::mem::size_of::<BackdropKernel>(), 800);
        assert!(BackdropKernel::new(2., true).count <= 7);
        for sigma in [0., -1., f32::NAN, f32::INFINITY] {
            assert_eq!(BackdropKernel::new(sigma, true).count, 0);
        }
        assert_eq!(BackdropKernel::new(1., false).count, 0);
        for sigma in [8.01, 16., 40., 128.] {
            assert_eq!(BackdropKernel::new(sigma, true).count, 49);
        }
    }
}
