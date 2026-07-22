use desklink_protocol::DirectLanCandidate;

use crate::{VideoPathKind, VideoPathQuality};

pub const DIRECT_VIDEO_PROBE_TIMEOUT_S: u64 = 3;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DirectVideoPathState {
    Relay,
    Offering {
        candidate: DirectLanCandidate,
    },
    Probing {
        candidate: DirectLanCandidate,
        deadline_unix_s: u64,
    },
    Direct {
        candidate_id: u64,
        allows_experimental_4k: bool,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DirectVideoPathFallbackReason {
    InvalidCandidate,
    Rejected,
    ProbeFailed,
    TimedOut,
    Stopped,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DirectVideoPathAction {
    SendOffer(DirectLanCandidate),
    SendAnswer {
        candidate_id: u64,
        accepted: bool,
        candidate: Option<DirectLanCandidate>,
    },
    StartProbe {
        candidate: DirectLanCandidate,
        deadline_unix_s: u64,
    },
    ActivateDirect {
        candidate_id: u64,
        allows_experimental_4k: bool,
    },
    UseRelay {
        reason: DirectVideoPathFallbackReason,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DirectVideoPathEvent {
    StartOffer {
        candidate: DirectLanCandidate,
        now_unix_s: u64,
    },
    ReceiveOffer {
        candidate: DirectLanCandidate,
        local_candidate: Option<DirectLanCandidate>,
        now_unix_s: u64,
    },
    ReceiveAnswer {
        candidate_id: u64,
        accepted: bool,
        candidate: Option<DirectLanCandidate>,
        now_unix_s: u64,
    },
    ProbeSucceeded {
        candidate_id: u64,
        quality: VideoPathQuality,
    },
    ProbeFailed {
        candidate_id: u64,
    },
    Tick {
        now_unix_s: u64,
    },
    Stop,
}

pub struct DirectVideoPathMachine {
    session_binding: [u8; 16],
    direct_probe_available: bool,
    state: DirectVideoPathState,
}

impl DirectVideoPathMachine {
    pub fn new(session_binding: [u8; 16]) -> Self {
        Self {
            session_binding,
            direct_probe_available: false,
            state: DirectVideoPathState::Relay,
        }
    }

    /// Enables the state machine only after a real authenticated direct
    /// probe implementation has been attached. Keeping this opt-in prevents
    /// the control plane from claiming a direct path while the data plane is
    /// still relay-only.
    pub const fn with_direct_probe(mut self) -> Self {
        self.direct_probe_available = true;
        self
    }

    pub const fn state(&self) -> &DirectVideoPathState {
        &self.state
    }

    pub fn apply(&mut self, event: DirectVideoPathEvent) -> Vec<DirectVideoPathAction> {
        use DirectVideoPathAction::*;
        use DirectVideoPathEvent::*;

        match event {
            StartOffer {
                candidate,
                now_unix_s,
            } => {
                if !self.direct_probe_available {
                    self.state = DirectVideoPathState::Relay;
                    return vec![UseRelay {
                        reason: DirectVideoPathFallbackReason::Rejected,
                    }];
                }
                if candidate
                    .validate(now_unix_s, &self.session_binding)
                    .is_err()
                {
                    self.state = DirectVideoPathState::Relay;
                    return vec![UseRelay {
                        reason: DirectVideoPathFallbackReason::InvalidCandidate,
                    }];
                }
                let action = SendOffer(candidate.clone());
                self.state = DirectVideoPathState::Offering { candidate };
                vec![action]
            }
            ReceiveOffer {
                candidate,
                local_candidate,
                now_unix_s,
            } => {
                if !self.direct_probe_available {
                    self.state = DirectVideoPathState::Relay;
                    return vec![
                        SendAnswer {
                            candidate_id: candidate.candidate_id(),
                            accepted: false,
                            candidate: None,
                        },
                        UseRelay {
                            reason: DirectVideoPathFallbackReason::Rejected,
                        },
                    ];
                }
                if candidate
                    .validate(now_unix_s, &self.session_binding)
                    .is_err()
                {
                    return vec![SendAnswer {
                        candidate_id: candidate.candidate_id(),
                        accepted: false,
                        candidate: None,
                    }];
                }
                let Some(local_candidate) = local_candidate.filter(|candidate| {
                    candidate
                        .validate(now_unix_s, &self.session_binding)
                        .is_ok()
                }) else {
                    return vec![SendAnswer {
                        candidate_id: candidate.candidate_id(),
                        accepted: false,
                        candidate: None,
                    }];
                };
                let deadline_unix_s = candidate
                    .expires_at_unix_s()
                    .min(now_unix_s.saturating_add(DIRECT_VIDEO_PROBE_TIMEOUT_S));
                let candidate_id = candidate.candidate_id();
                self.state = DirectVideoPathState::Probing {
                    candidate: candidate.clone(),
                    deadline_unix_s,
                };
                vec![
                    SendAnswer {
                        candidate_id,
                        accepted: true,
                        candidate: Some(local_candidate),
                    },
                    StartProbe {
                        candidate,
                        deadline_unix_s,
                    },
                ]
            }
            ReceiveAnswer {
                candidate_id,
                accepted,
                candidate: remote_candidate,
                now_unix_s,
            } => {
                let DirectVideoPathState::Offering { candidate } = &self.state else {
                    return Vec::new();
                };
                if candidate.candidate_id() != candidate_id {
                    return Vec::new();
                }
                if !accepted {
                    self.state = DirectVideoPathState::Relay;
                    return vec![UseRelay {
                        reason: DirectVideoPathFallbackReason::Rejected,
                    }];
                }
                let Some(remote_candidate) = remote_candidate else {
                    self.state = DirectVideoPathState::Relay;
                    return vec![UseRelay {
                        reason: DirectVideoPathFallbackReason::InvalidCandidate,
                    }];
                };
                if remote_candidate
                    .validate(now_unix_s, &self.session_binding)
                    .is_err()
                {
                    self.state = DirectVideoPathState::Relay;
                    return vec![UseRelay {
                        reason: DirectVideoPathFallbackReason::TimedOut,
                    }];
                }
                let remote_candidate = remote_candidate.clone();
                let deadline_unix_s = remote_candidate
                    .expires_at_unix_s()
                    .min(now_unix_s.saturating_add(DIRECT_VIDEO_PROBE_TIMEOUT_S));
                self.state = DirectVideoPathState::Probing {
                    candidate: remote_candidate.clone(),
                    deadline_unix_s,
                };
                vec![StartProbe {
                    candidate: remote_candidate,
                    deadline_unix_s,
                }]
            }
            ProbeSucceeded {
                candidate_id,
                quality,
            } => {
                let matches = matches!(
                    &self.state,
                    DirectVideoPathState::Probing { candidate, .. }
                        if candidate.candidate_id() == candidate_id
                );
                if !matches || quality.kind != VideoPathKind::DirectLan {
                    return if matches {
                        self.fallback(DirectVideoPathFallbackReason::ProbeFailed)
                    } else {
                        Vec::new()
                    };
                }
                let allows_experimental_4k = quality.allows_experimental_4k();
                self.state = DirectVideoPathState::Direct {
                    candidate_id,
                    allows_experimental_4k,
                };
                vec![ActivateDirect {
                    candidate_id,
                    allows_experimental_4k,
                }]
            }
            ProbeFailed { candidate_id } => {
                let matches = matches!(
                    &self.state,
                    DirectVideoPathState::Probing { candidate, .. }
                        if candidate.candidate_id() == candidate_id
                );
                if matches {
                    self.fallback(DirectVideoPathFallbackReason::ProbeFailed)
                } else {
                    Vec::new()
                }
            }
            Tick { now_unix_s } => {
                let timed_out = matches!(
                    &self.state,
                    DirectVideoPathState::Offering { candidate }
                        if now_unix_s >= candidate.expires_at_unix_s()
                ) || matches!(
                    &self.state,
                    DirectVideoPathState::Probing { deadline_unix_s, .. }
                        if now_unix_s >= *deadline_unix_s
                );
                if timed_out {
                    self.fallback(DirectVideoPathFallbackReason::TimedOut)
                } else {
                    Vec::new()
                }
            }
            Stop => {
                if self.state == DirectVideoPathState::Relay {
                    Vec::new()
                } else {
                    self.fallback(DirectVideoPathFallbackReason::Stopped)
                }
            }
        }
    }

    fn fallback(&mut self, reason: DirectVideoPathFallbackReason) -> Vec<DirectVideoPathAction> {
        self.state = DirectVideoPathState::Relay;
        vec![DirectVideoPathAction::UseRelay { reason }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{net::SocketAddr, sync::Arc, time::Duration};

    fn candidate_with_id(candidate_id: u64, binding: [u8; 16]) -> DirectLanCandidate {
        DirectLanCandidate::new(
            candidate_id,
            SocketAddr::from(([192, 168, 1, 20], 45_100)),
            110,
            binding,
            100,
        )
        .expect("candidate")
    }

    fn candidate(binding: [u8; 16]) -> DirectLanCandidate {
        candidate_with_id(7, binding)
    }

    fn good_quality() -> VideoPathQuality {
        VideoPathQuality {
            kind: VideoPathKind::DirectLan,
            rtt_ms: 20,
            loss_basis_points: 10,
        }
    }

    #[test]
    fn offer_answer_probe_and_activation_are_explicit() {
        let binding = [4; 16];
        let mut machine = DirectVideoPathMachine::new(binding).with_direct_probe();
        let local = candidate(binding);
        assert_eq!(
            machine.apply(DirectVideoPathEvent::StartOffer {
                candidate: local.clone(),
                now_unix_s: 100,
            }),
            vec![DirectVideoPathAction::SendOffer(local.clone())]
        );
        assert!(matches!(
            machine.state(),
            DirectVideoPathState::Offering { candidate } if candidate.candidate_id() == 7
        ));
        let remote = candidate_with_id(8, binding);
        assert_eq!(
            machine.apply(DirectVideoPathEvent::ReceiveAnswer {
                candidate_id: 7,
                accepted: true,
                candidate: Some(remote.clone()),
                now_unix_s: 101,
            }),
            vec![DirectVideoPathAction::StartProbe {
                candidate: remote,
                deadline_unix_s: 104,
            }]
        );
        assert_eq!(
            machine.apply(DirectVideoPathEvent::ProbeSucceeded {
                candidate_id: 8,
                quality: good_quality(),
            }),
            vec![DirectVideoPathAction::ActivateDirect {
                candidate_id: 8,
                allows_experimental_4k: true,
            }]
        );
        assert_eq!(
            machine.state(),
            &DirectVideoPathState::Direct {
                candidate_id: 8,
                allows_experimental_4k: true,
            }
        );
    }

    #[test]
    fn received_offer_is_rejected_when_it_belongs_to_another_session() {
        let mut machine = DirectVideoPathMachine::new([4; 16]).with_direct_probe();
        let actions = machine.apply(DirectVideoPathEvent::ReceiveOffer {
            candidate: candidate([5; 16]),
            local_candidate: Some(candidate([4; 16])),
            now_unix_s: 100,
        });
        assert_eq!(
            actions,
            vec![DirectVideoPathAction::SendAnswer {
                candidate_id: 7,
                accepted: false,
                candidate: None,
            }]
        );
        assert_eq!(machine.state(), &DirectVideoPathState::Relay);
    }

    #[test]
    fn timeout_or_probe_failure_falls_back_without_touching_control_state() {
        let binding = [6; 16];
        let mut machine = DirectVideoPathMachine::new(binding).with_direct_probe();
        let local = candidate(binding);
        machine.apply(DirectVideoPathEvent::StartOffer {
            candidate: local,
            now_unix_s: 100,
        });
        assert_eq!(
            machine.apply(DirectVideoPathEvent::Tick { now_unix_s: 110 }),
            vec![DirectVideoPathAction::UseRelay {
                reason: DirectVideoPathFallbackReason::TimedOut,
            }]
        );
        assert_eq!(machine.state(), &DirectVideoPathState::Relay);
    }

    #[test]
    fn relay_quality_cannot_activate_direct_video() {
        let binding = [8; 16];
        let mut machine = DirectVideoPathMachine::new(binding).with_direct_probe();
        machine.apply(DirectVideoPathEvent::ReceiveOffer {
            candidate: candidate(binding),
            local_candidate: Some(candidate(binding)),
            now_unix_s: 100,
        });
        assert_eq!(
            machine.apply(DirectVideoPathEvent::ProbeSucceeded {
                candidate_id: 7,
                quality: VideoPathQuality {
                    kind: VideoPathKind::Relay,
                    rtt_ms: 20,
                    loss_basis_points: 0,
                },
            }),
            vec![DirectVideoPathAction::UseRelay {
                reason: DirectVideoPathFallbackReason::ProbeFailed,
            }]
        );
        assert_eq!(machine.state(), &DirectVideoPathState::Relay);
    }

    #[test]
    fn relay_only_machine_rejects_candidates_until_a_probe_is_attached() {
        let binding = [3; 16];
        let mut machine = DirectVideoPathMachine::new(binding);
        let local = candidate(binding);

        assert_eq!(
            machine.apply(DirectVideoPathEvent::StartOffer {
                candidate: local.clone(),
                now_unix_s: 100,
            }),
            vec![DirectVideoPathAction::UseRelay {
                reason: DirectVideoPathFallbackReason::Rejected,
            }]
        );
        assert_eq!(
            machine.apply(DirectVideoPathEvent::ReceiveOffer {
                candidate: local,
                local_candidate: None,
                now_unix_s: 100,
            }),
            vec![
                DirectVideoPathAction::SendAnswer {
                    candidate_id: 7,
                    accepted: false,
                    candidate: None,
                },
                DirectVideoPathAction::UseRelay {
                    reason: DirectVideoPathFallbackReason::Rejected,
                },
            ]
        );
        assert_eq!(machine.state(), &DirectVideoPathState::Relay);
    }

    #[tokio::test]
    async fn authenticated_probe_activates_the_direct_path_machine() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let binding = [11; 16];
        let offerer_endpoint =
            Arc::new(crate::DirectLanEndpoint::bind("127.0.0.1:0".parse().unwrap()).unwrap());
        let responder_endpoint =
            crate::DirectLanEndpoint::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let offerer_candidate = DirectLanCandidate::new(
            41,
            offerer_endpoint.local_addr().unwrap(),
            110,
            binding,
            100,
        )
        .unwrap();
        let responder_candidate = DirectLanCandidate::new(
            42,
            responder_endpoint.local_addr().unwrap(),
            110,
            binding,
            100,
        )
        .unwrap();
        let mut machine = DirectVideoPathMachine::new(binding).with_direct_probe();
        assert_eq!(
            machine.apply(DirectVideoPathEvent::ReceiveOffer {
                candidate: offerer_candidate.clone(),
                local_candidate: Some(responder_candidate),
                now_unix_s: 100,
            }),
            vec![
                DirectVideoPathAction::SendAnswer {
                    candidate_id: 41,
                    accepted: true,
                    candidate: Some(
                        DirectLanCandidate::new(
                            42,
                            responder_endpoint.local_addr().unwrap(),
                            110,
                            binding,
                            100,
                        )
                        .unwrap()
                    ),
                },
                DirectVideoPathAction::StartProbe {
                    candidate: offerer_candidate.clone(),
                    deadline_unix_s: 103,
                },
            ]
        );

        let (mut offerer_secure, mut responder_secure) = connected_secure_sessions();
        let offerer_for_task = offerer_endpoint.clone();
        let accept_task = tokio::spawn(async move {
            let incoming = offerer_for_task.accept().await.unwrap().unwrap();
            offerer_for_task
                .accept_probe_connection(incoming, 41, &binding, &mut offerer_secure, 100)
                .await
        });
        let (responder_connection, probe) = responder_endpoint
            .connect(&offerer_candidate, &binding, &mut responder_secure, 100)
            .await
            .unwrap();
        let (offerer_connection, _) = accept_task.await.unwrap().unwrap();
        assert_eq!(probe.candidate_id, 41);
        assert!(probe.rtt_ms < 3_000);
        assert_eq!(
            machine.apply(DirectVideoPathEvent::ProbeSucceeded {
                candidate_id: probe.candidate_id,
                quality: VideoPathQuality {
                    kind: VideoPathKind::DirectLan,
                    rtt_ms: probe.rtt_ms,
                    loss_basis_points: u16::MAX,
                },
            }),
            vec![DirectVideoPathAction::ActivateDirect {
                candidate_id: 41,
                allows_experimental_4k: false,
            }]
        );
        assert_eq!(
            machine.state(),
            &DirectVideoPathState::Direct {
                candidate_id: 41,
                allows_experimental_4k: false,
            }
        );
        responder_connection.send_datagram(vec![7, 8, 9]).unwrap();
        let datagram =
            tokio::time::timeout(Duration::from_secs(1), offerer_connection.recv_datagram())
                .await
                .unwrap()
                .unwrap();
        assert_eq!(datagram, vec![7, 8, 9]);
    }

    fn connected_secure_sessions() -> (
        desklink_crypto::SecureSession,
        desklink_crypto::SecureSession,
    ) {
        use desklink_crypto::{DeviceIdentity, NoiseInitiator, NoiseResponder, SecureRole};
        use rand_chacha::ChaCha20Rng;
        use rand_core::SeedableRng;

        let initiator_identity = DeviceIdentity::generate(&mut ChaCha20Rng::from_seed([1; 32]));
        let responder_identity = DeviceIdentity::generate(&mut ChaCha20Rng::from_seed([2; 32]));
        let initiator_verify_key = initiator_identity.verify_key();
        let responder_verify_key = responder_identity.verify_key();
        let (mut initiator, message_1) =
            NoiseInitiator::start(initiator_identity, responder_verify_key).unwrap();
        let (mut responder, message_2) =
            NoiseResponder::accept(&message_1, responder_identity, initiator_verify_key).unwrap();
        let message_3 = initiator.receive(&message_2).unwrap();
        responder.receive(&message_3).unwrap();
        (
            initiator
                .finish()
                .unwrap()
                .into_secure_session(SecureRole::Initiator),
            responder
                .finish()
                .unwrap()
                .into_secure_session(SecureRole::Responder),
        )
    }
}
