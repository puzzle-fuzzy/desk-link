// Compile the queue independently of the host's capture/control integration.
// The same source is used by the production runtime once its lane is wired.
#[path = "../src/runtime/configuration_queue.rs"]
mod configuration_queue;
