//! Closed-form damped spring, sampled only by the existing native motion clock.
//! duration is a settling horizon; bounce 0 is critically damped, 1 is most bouncy.
//! This is a duration/bounce API, not a claim of Apple's private spring formula.
pub(crate) fn progress(t: f64, bounce: f64) -> f64 {
    if t <= 0.0 {
        return 0.0;
    }
    if t >= 1.0 {
        return 1.0;
    }
    let damping = (1.0 - bounce).max(0.1);
    if damping >= 1.0 {
        let x = 12.0 * t;
        1.0 - (1.0 + x) * (-x).exp()
    } else {
        let omega = 8.0 / damping;
        let damped = omega * (1.0 - damping * damping).sqrt();
        1.0 - (-damping * omega * t).exp() * ((damped * t).cos() + damping * omega / damped * (damped * t).sin())
    }
}
#[cfg(test)]
mod tests {
    use super::velocity_offset;
    #[test]
    fn initial_velocity_and_equal_endpoints() {
        for bounce in [0.0, 0.4] {
            let dt = 1e-7;
            assert!((velocity_offset(dt, bounce, 400.0, 0.5) / (dt * 0.5) - 400.0).abs() < 0.01);
            assert_eq!(velocity_offset(0.0, bounce, 400.0, 0.5), 0.0);
            assert_eq!(velocity_offset(1.0, bounce, 400.0, 0.5), 0.0);
            assert!(velocity_offset(0.1, bounce, -400.0, 0.5) < 0.0);
        }
    }
    #[test]
    fn endpoints_and_overshoot() {
        for bounce in [0.0, 0.2, 0.4, 1.0] {
            assert_eq!(super::progress(0.0, bounce), 0.0);
            assert_eq!(super::progress(1.0, bounce), 1.0);
            for n in 0..=100 {
                assert!(super::progress(n as f64 / 100.0, bounce).is_finite());
            }
        }
        assert!(super::progress(0.3, 0.4) > 1.0);
        assert!(super::progress(0.3, 0.0) < 1.0);
    }
}

/// Homogeneous response with zero displacement and explicit units/second velocity.
/// Unlike normalized velocity this is well-defined when source == target.
pub(crate) fn velocity_offset(t: f64, bounce: f64, velocity: f64, duration: f64) -> f64 {
    if t <= 0.0 || t >= 1.0 {
        return 0.0;
    }
    let damping = (1.0 - bounce).max(0.1);
    let response = if damping >= 1.0 {
        t * (-12.0 * t).exp()
    } else {
        let omega = 8.0 / damping;
        let damped = omega * (1.0 - damping * damping).sqrt();
        (-damping * omega * t).exp() * (damped * t).sin() / damped
    };
    velocity * duration * response
}
