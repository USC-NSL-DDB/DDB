// Inject the debugger's attach signal while discovery publication is in flight.
// MQTT is stubbed at its public C boundary; production connector code is used.
#include "ddb/integration.hpp"
#include <cassert>
#include <cstdlib>

static volatile sig_atomic_t traps = 0;
static bool fail_connect = false;

extern "C" {
int MQTTClient_create(MQTTClient* handle, const char*, const char*, int, void*) {
  *handle = nullptr;
  return MQTTCLIENT_SUCCESS;
}
int MQTTClient_connect(MQTTClient, MQTTClient_connectOptions*) {
  return fail_connect ? MQTTCLIENT_FAILURE : MQTTCLIENT_SUCCESS;
}
int MQTTClient_publishMessage(MQTTClient, const char*, MQTTClient_message*,
                              MQTTClient_deliveryToken* token) {
  *token = 1;
  raise(40); // A fast backend can attach before publication returns.
  return MQTTCLIENT_SUCCESS;
}
int MQTTClient_waitForCompletion(MQTTClient, MQTTClient_deliveryToken, unsigned long) {
  return MQTTCLIENT_SUCCESS;
}
int MQTTClient_disconnect(MQTTClient, int) { return MQTTCLIENT_SUCCESS; }
void MQTTClient_destroy(MQTTClient*) {}
}

static bool attach_signal_blocked() {
  sigset_t mask;
  assert(pthread_sigmask(SIG_BLOCK, nullptr, &mask) == 0);
  return sigismember(&mask, 40) == 1;
}

int main() {
  alarm(3); // A lost handshake must fail rather than hang the build.
  struct sigaction action{};
  action.sa_handler = [](int) { ++traps; };
  sigemptyset(&action.sa_mask);
  assert(sigaction(SIGTRAP, &action, nullptr) == 0);
  char config_path[] = "/tmp/ddb-attach-signal-XXXXXX";
  int descriptor = mkstemp(config_path);
  assert(descriptor >= 0);
  close(descriptor);
  std::ofstream(config_path) << "tcp://unused\ndiscovery\n";
  auto config = DDB::Config::get_default("127.0.0.1").with_ini_filepath(config_path)
                    .with_hash("signal-order-test");
  {
    DDB::DDBConnector connector(config);
    connector.init();
    assert(traps == 1);
    assert(attach_signal_blocked());
  }
  assert(!attach_signal_blocked());
  config.wait_for_attach = false;
  {
    DDB::DDBConnector connector(config);
    connector.init();
    assert(traps == 2);
  }
  config.wait_for_attach = true;
  fail_connect = true;
  {
    DDB::DDBConnector connector(config);
    connector.init();
    assert(!attach_signal_blocked());
  }
  unlink(config_path);
  alarm(0);
  std::cout << "connector attach signal-order regression passed\n";
}
