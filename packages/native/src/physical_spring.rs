//! Closed-form mass/stiffness/damping response; time is seconds, never frame steps.
//! Kept separate from the existing duration/bounce API.
pub(crate) fn physical_response(t: f64, stiffness: f64, damping: f64, mass: f64) -> f64 {
    let omega = (stiffness / mass).sqrt();
    let decay = damping / (2.0 * mass);
    let discriminant = decay * decay - omega * omega;
    if discriminant.abs() <= omega * omega * 1e-8 {
        1.0 - (1.0 + omega * t) * (-omega * t).exp()
    } else if discriminant < 0.0 {
        let frequency = (-discriminant).sqrt();
        1.0 - (-decay * t).exp() * ((frequency * t).cos() + decay / frequency * (frequency * t).sin())
    } else {
        let root = discriminant.sqrt();
        let slow = -omega * omega / (decay + root);
        let fast = -decay - root;
        1.0 + (fast * (slow * t).exp() - slow * (fast * t).exp()) / (slow - fast)
    }
}
pub(crate) fn physical_velocity(t: f64, stiffness: f64, damping: f64, mass: f64, velocity: f64) -> f64 {
    let omega = (stiffness / mass).sqrt();
    let decay = damping / (2.0 * mass);
    let discriminant = decay * decay - omega * omega;
    let response = if discriminant.abs() <= omega * omega * 1e-8 {
        t * (-omega * t).exp()
    } else if discriminant < 0.0 {
        let frequency = (-discriminant).sqrt();
        (-decay * t).exp() * (frequency * t).sin() / frequency
    } else {
        let root = discriminant.sqrt();
        let slow = -omega * omega / (decay + root);
        let fast = -decay - root;
        ((slow * t).exp() - (fast * t).exp()) / (slow - fast)
    };
    velocity * response
}
/// Conservative envelope horizon: normalized displacement < .001 and velocity
/// < .01/s. An underdamped crossing cannot incorrectly end the animation early.
pub(crate) fn physical_horizon(stiffness: f64, damping: f64, mass: f64) -> f64 {
    let omega = (stiffness / mass).sqrt();
    let decay = damping / (2.0 * mass);
    let discriminant = decay * decay - omega * omega;
    if discriminant.abs() <= omega * omega * 1e-8 {
        let mut t = 1.0 / omega;
        while (1.0 + omega * t) * (-omega * t).exp() > 0.001 || omega * omega * t * (-omega * t).exp() > 0.01 {
            t += 1.0 / (120.0 * omega);
        }
        t
    } else if discriminant < 0.0 {
        let frequency = (-discriminant).sqrt();
        let amplitude = (1.0 + (decay / frequency).powi(2)).sqrt();
        ((amplitude / 0.001).ln() / decay).max((amplitude * omega / 0.01).ln() / decay)
    } else {
        let root = discriminant.sqrt();
        let slow = -omega * omega / (decay + root);
        let fast = -decay - root;
        let amplitude = (fast.abs() + slow.abs()) / (slow - fast);
        ((amplitude / 0.001).ln() / -slow).max((2.0 * slow * fast / (slow - fast) / 0.01).ln() / -slow)
    }
}
#[cfg(test)]
mod physical_tests {
    #[test]
    fn desktop_parameters_and_damping_regimes() {
        for (k, c, m) in [(360., 32., 0.6), (460., 30., 0.55), (100., 20., 1.), (100., 40., 1.)] {
            assert_eq!(super::physical_response(0., k, c, m), 0.);
            let dt = 1e-7;
            assert!((super::physical_velocity(dt, k, c, m, 400.) / dt - 400.).abs() < 0.01);
            let horizon = super::physical_horizon(k, c, m);
            for step in 0..120 {
                let t = horizon + step as f64 / 120.;
                assert!((super::physical_response(t, k, c, m) - 1.).abs() < 0.001);
            }
            assert!(horizon < 4.);
            if k > 300. {
                assert!(horizon < 1.);
            }
        }
        // Independent critically damped solution: k=100, c=20, m=1.
        assert!((super::physical_response(0.1, 100., 20., 1.) - (1. - 2. / std::f64::consts::E)).abs() < 1e-12);
    }
}
