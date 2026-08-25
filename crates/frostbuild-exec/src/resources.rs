//! Resource admission shared by the real executor and schedule simulation.
//!
//! Keeping one state machine is correctness, not tidiness: a simulator whose
//! fit rule differs from the executor can emit precise-looking makespans for a
//! schedule that can never actually run.

use frostbuild_core::manifest::ActionResources;

use crate::ResourceLimits;

#[derive(Debug, Clone, Copy)]
pub(crate) struct ResourceAdmission {
    limits: ResourceLimits,
    cpu: usize,
    ram_mb: u64,
    tests: usize,
    running: usize,
    exclusive: bool,
    peak_cpu: usize,
    peak_ram_mb: u64,
    peak_tests: usize,
}

impl ResourceAdmission {
    pub(crate) fn new(limits: ResourceLimits) -> Self {
        Self {
            limits: ResourceLimits {
                cpu: limits.cpu.max(1),
                ram_mb: limits.ram_mb.max(1),
                test_jobs: limits.test_jobs.max(1),
            },
            cpu: 0,
            ram_mb: 0,
            tests: 0,
            running: 0,
            exclusive: false,
            peak_cpu: 0,
            peak_ram_mb: 0,
            peak_tests: 0,
        }
    }

    /// Oversized work consumes the whole corresponding pool. It therefore
    /// runs alone with respect to that resource instead of waiting forever.
    fn demand(&self, resources: ActionResources) -> (usize, u64) {
        (
            (resources.cpu as usize).min(self.limits.cpu),
            resources.ram_mb.min(self.limits.ram_mb),
        )
    }

    pub(crate) fn fits(&self, resources: ActionResources, is_test: bool) -> bool {
        if resources.exclusive {
            return self.running == 0;
        }
        if self.exclusive || (is_test && self.tests >= self.limits.test_jobs) {
            return false;
        }
        let (cpu, ram_mb) = self.demand(resources);
        self.cpu.saturating_add(cpu) <= self.limits.cpu
            && self.ram_mb.saturating_add(ram_mb) <= self.limits.ram_mb
    }

    pub(crate) fn reserve(&mut self, resources: ActionResources, is_test: bool) {
        debug_assert!(self.fits(resources, is_test));
        let (cpu, ram_mb) = self.demand(resources);
        self.cpu += cpu;
        self.ram_mb += ram_mb;
        self.tests += usize::from(is_test);
        self.running += 1;
        self.exclusive = resources.exclusive;
        self.peak_cpu = self.peak_cpu.max(self.cpu);
        self.peak_ram_mb = self.peak_ram_mb.max(self.ram_mb);
        self.peak_tests = self.peak_tests.max(self.tests);
    }

    pub(crate) fn release(&mut self, resources: ActionResources, is_test: bool) {
        let (cpu, ram_mb) = self.demand(resources);
        self.cpu -= cpu;
        self.ram_mb -= ram_mb;
        self.tests -= usize::from(is_test);
        self.running -= 1;
        if resources.exclusive {
            self.exclusive = false;
        }
    }

    pub(crate) fn peak_cpu(&self) -> usize {
        self.peak_cpu
    }

    pub(crate) fn peak_ram_mb(&self) -> u64 {
        self.peak_ram_mb
    }

    pub(crate) fn peak_tests(&self) -> usize {
        self.peak_tests
    }
}
