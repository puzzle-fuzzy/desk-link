"""Portable scheduling contracts; real flow-control coverage lives in Rust."""
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[2]


def source(path):
    return (ROOT / path).read_text(encoding="utf-8")


class WorkerReceiveContracts(unittest.TestCase):
    def test_reader_is_pinned_across_commands_and_send_completion(self):
        worker = source("crates/desklink-ffi/src/worker.rs")
        connected = worker.split("async fn run_connected(", 1)[1].split("/// Every owned reliable operation", 1)[0]
        self.assertEqual(connected.count("controller.next_event()"), 1)
        self.assertLess(connected.index("tokio::pin!(next_event)"), connected.index("let event = loop"))
        self.assertIn("event = &mut next_event", connected)
        self.assertIn("ConnectedWork::Sent(result)", connected)
        self.assertIn("ConnectedWork::Command(command)", connected)
        self.assertNotIn("controller.send_", connected)
        fair = connected.split("work = async {", 1)[1].split("} => work", 1)[0]
        self.assertNotIn("biased;", fair)
        self.assertIn("changed = cancellation.changed()", connected)
        before_result = connected.split("} => work,", 1)[1].split("match work {", 1)[0]
        self.assertIn("if *cancellation.borrow()", before_result)
        self.assertIn("ConnectedOutcome::Shutdown", before_result)

    def test_current_geometry_filters_only_unstarted_input(self):
        worker = source("crates/desklink-ffi/src/worker.rs")
        self.assertIn("let mut input_context: Option<InputContext> = None", worker)
        self.assertIn("Some(input.context) == input_context", worker)
        self.assertIn("!send.started && send.context.is_some_and(|context| context != current)", worker)
        self.assertIn("this.started = true", worker)
        self.assertIn("discard_unstarted_input(&mut sending, current)", worker)
        self.assertIn("input.context, input.event", worker)

    def test_release_queues_are_bounded_and_overflow_cancels_the_session(self):
        worker = source("crates/desklink-ffi/src/worker.rs")
        self.assertIn("const MAX_RELEASE_EVENTS: usize = COMMAND_CAPACITY", worker)
        self.assertIn("pending.len().saturating_add(events.len()) > MAX_RELEASE_EVENTS", worker)
        self.assertIn("initial.len() > MAX_RELEASE_EVENTS", worker)
        self.assertIn("events.len() > MAX_RELEASE_EVENTS", worker)
        release = worker.split("pub(crate) fn release_all(", 1)[1].split("pub(crate) fn is_finished", 1)[0]
        self.assertIn("if result.is_err()", release)
        self.assertIn("self.cancellation.send(true)", release)
        self.assertIn("senders.control.close()", worker)

    def test_real_flow_limits_reach_endpoints_and_default_constructors_remain_default(self):
        transport = source("crates/desklink-transport/src/lib.rs")
        self.assertIn("flow_control_limits: None", transport)
        self.assertIn("stream_receive_window > receive_window", transport)
        self.assertIn("window == 0 || window > Self::MAX_WINDOW_BYTES", transport)
        client = source("crates/desklink-transport/src/quic.rs").split("pub async fn connect(config:", 1)[1].split("let (control_sender", 1)[0]
        self.assertLess(client.index("limits.apply_to(&mut transport)"), client.index(".keep_alive_interval("))
        self.assertLess(client.index("limits.apply_to(&mut transport)"), client.index("endpoint.set_default_client_config(client_config)"))
        relay = source("server/relay/src/lib.rs")
        ordinary = relay.split("pub async fn bind(", 1)[1].split("pub async fn bind_with_transport", 1)[0]
        self.assertIn("quinn::TransportConfig::default()", ordinary)
        explicit = relay.split("pub async fn bind_with_transport", 1)[1].split("pub fn local_addr", 1)[0]
        self.assertNotIn("TransportConfig::default()", explicit)
        self.assertIn("server_config.transport_config(Arc::new(transport))", explicit)
        self.assertIn("quinn::Endpoint::server(server_config, bind_addr)", explicit)
        fixture = source("crates/desklink-ffi/src/controller/direct_silence_tests.rs")
        self.assertIn("with_flow_control_limits(fixture_flow_control_limits())", fixture)
        self.assertIn("fixture_flow_control_limits().apply_to(&mut transport)", fixture)
        self.assertIn("QuicFlowControlLimits::new(2048, 16384, 2048)", fixture)
        self.assertIn("RelayServer::bind_with_transport", fixture)

    def test_owned_control_and_runtime_recovery_share_wire_order_without_crypto_io_lock(self):
        controller = source("crates/desklink-ffi/src/controller.rs")
        owned = controller.split("impl ControllerControlSender", 1)[1].split("pub struct ControllerTransferSender", 1)[0]
        self.assertLess(owned.index("self.order.lock().await"), owned.index("send_control(&self.client"))
        keyframe = controller.split("async fn request_keyframe_for", 1)[1].split("fn drop_video_packet", 1)[0]
        self.assertIn("self.send_controller_control(ControlMessage::RequestKeyframe", keyframe)
        self.assertIn("order: self.control_order.clone()", controller)
        helper = controller.split("async fn send_control(", 1)[1].split("async fn send_session_input", 1)[0]
        self.assertIn("seal(secure, SecureLane::Control, &plaintext).await?", helper)
        self.assertNotIn("secure.lock()", helper)
        self.assertIn("tokio::time::timeout(CONTROLLER_SEND_TIMEOUT, async", owned)
        self.assertIn("self.close()", owned)
        self.assertIn("control send deadline exceeded", owned)
        worker = source("crates/desklink-ffi/src/worker.rs")
        self.assertIn("const SEND_TIMEOUT: Duration = crate::controller::CONTROLLER_SEND_TIMEOUT", worker)
        self.assertIn("CONTROLLER_SEND_TIMEOUT: Duration = Duration::from_secs(1)", controller)


if __name__ == "__main__":
    unittest.main()
