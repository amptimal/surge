// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0

use surge_network::Network;
use surge_network::network::Branch;
use surge_network::network::{Bus, BusType, Generator, Load};

pub fn two_bus_test_network(slack_bus: u32, pq_bus: u32, load_mw: f64) -> Network {
    let mut net = Network::new("hvdc-test-2bus");
    net.base_mva = 100.0;

    let mut b1 = Bus::new(slack_bus, BusType::Slack, 230.0);
    b1.voltage_magnitude_pu = 1.0;
    b1.voltage_angle_rad = 0.0;
    net.buses.push(b1);

    let b2 = Bus::new(pq_bus, BusType::PQ, 230.0);
    net.buses.push(b2);
    net.loads.push(Load::new(pq_bus, load_mw, 0.0));

    net.branches
        .push(Branch::new_line(slack_bus, pq_bus, 0.01, 0.05, 0.02));

    let mut slack = Generator::new(slack_bus, load_mw * 1.5, 1.0);
    slack.pmax = load_mw * 3.0;
    slack.qmax = load_mw * 2.0;
    slack.qmin = -load_mw * 2.0;
    net.generators.push(slack);

    net
}
