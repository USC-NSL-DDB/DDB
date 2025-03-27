#pragma once

#include <pthread.h>
#include <signal.h>
#include <stdbool.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

#include "cddb/basic.h"
#include "cddb/service_reporter.h"

#ifdef __cplusplus
extern "C" {
#endif

#define SIGDDBWAIT 40 // re-use real-time signal for ddb needs

typedef struct {
  char *ipv4;
  bool auto_discovery;
  bool wait_for_attach;
  char *tag;
  char *alias;
  char *ini_filepath;
} DDBConfig;

typedef struct {
  DDBConfig config;
  DDBServiceReporter reporter;
} DDBConnector;

static inline DDBConfig ddb_config_get_default(const char *ipv4) {
  DDBConfig config;
  config.ipv4 = strdup(ipv4);
  config.auto_discovery = true;
  config.wait_for_attach = true;
  config.tag = strdup("proc");
  config.alias = strdup("bin");
  config.ini_filepath = strdup(ddb_default_ini_filepath());
  return config;
}

static inline DDBConfig ddb_config_get_default_local() {
  return ddb_config_get_default(ddb_uint32_to_ipv4(ddb_get_ipv4_from_local()));
}

static inline DDBConfig ddb_config_with_tag(DDBConfig *config,
                                            const char *tag) {
  DDBConfig new_config = *config;

  if (new_config.tag) {
    free(new_config.tag);
  }
  new_config.tag = strdup(tag);

  return new_config;
}

static inline DDBConfig ddb_config_with_alias(DDBConfig *config,
                                              const char *alias) {
  DDBConfig new_config = *config;

  if (new_config.alias) {
    free(new_config.alias);
  }
  new_config.alias = strdup(alias);

  return new_config;
}

static inline DDBConfig ddb_config_with_ini_filepath(DDBConfig *config,
                                                     const char *ini_filepath) {
  DDBConfig new_config = *config;

  if (new_config.ini_filepath) {
    free(new_config.ini_filepath);
  }
  new_config.ini_filepath = strdup(ini_filepath);

  return new_config;
}

static inline char *ddb_config_to_string(DDBConfig *config) {
  char buffer[1024];
  snprintf(buffer, sizeof(buffer),
           "Config { ipv4 = %s, auto_discovery = %s, wait_for_attach = %s, "
           "tag = %s, alias = %s, ini_filepath = %s }",
           config->ipv4 ? config->ipv4 : "NULL",
           config->auto_discovery ? "true" : "false",
           config->wait_for_attach ? "true" : "false",
           config->tag ? config->tag : "NULL",
           config->alias ? config->alias : "NULL",
           config->ini_filepath ? config->ini_filepath : "NULL");

  return strdup(buffer);
}

// DDBConnector *ddb_connector_create_with_ipv4(const char *ipv4,
//                                              bool enable_discovery);
// DDBConnector *ddb_connector_create_with_ipv4_and_wait(const char *ipv4,
//                                                       bool enable_discovery,
//                                                       bool wait_for_attach);

static inline void ddb_block_signal(int sig) {
  sigset_t set;
  sigemptyset(&set);
  sigaddset(&set, sig);
  pthread_sigmask(SIG_BLOCK, &set, NULL);
}

static inline void ddb_unblock_signal(int sig) {
  sigset_t set;
  sigemptyset(&set);
  sigaddset(&set, sig);
  pthread_sigmask(SIG_UNBLOCK, &set, NULL);
}

static inline void ddb_wait_for_signal(int sig) {
  sigset_t set;
  int received_sig;

  sigemptyset(&set);
  sigaddset(&set, sig);

  printf("Process PID: %d. Waiting for signal %d to continue...\n", getpid(),
         sig);
  sigwait(&set, &received_sig);
  printf("Debugger attached. Resume execution...\n");
}

static inline void ddb_sig_ddb_wait_handler(int signum) {
  (void)signum;
  raise(SIGTRAP);
}

void ddb_setup_signal_handler() {
  struct sigaction sig_ddb_wait_action;
  sig_ddb_wait_action.sa_handler = ddb_sig_ddb_wait_handler;
  sigemptyset(&sig_ddb_wait_action.sa_mask);
  sig_ddb_wait_action.sa_flags = 0;
  sigaction(SIGDDBWAIT, &sig_ddb_wait_action, NULL);
}

static inline void ddb_wait_for_debugger() {
  ddb_block_signal(SIGDDBWAIT);
  ddb_wait_for_signal(SIGDDBWAIT);
  raise(SIGTRAP);
}

static inline DDBConnector *ddb_connector_create() {
  DDBConnector *connector = (DDBConnector *)malloc(sizeof(DDBConnector));
  if (connector) {
    connector->config = ddb_config_get_default_local();
  }
  return connector;
}

static inline DDBConnector *ddb_connector_create_with_config(DDBConfig config) {
  DDBConnector *connector = (DDBConnector *)malloc(sizeof(DDBConnector));
  if (connector) {
    connector->config = config;
  }
  return connector;
}

static inline void ddb_init_discovery(DDBConnector *connector) {
  char hash[DDB_HASH_LEN];
  if (ddb_compute_self_hash(hash) != 0) {
    fprintf(stderr, "failed to compute self hash\n");
    return;
  }

  DDBServiceInfo service = {.ip = ddb_meta.comm_ip, .pid = ddb_meta.pid};

  // Copy tag
  strncpy(service.tag, connector->config.tag, sizeof(service.tag) - 1);
  service.tag[sizeof(service.tag) - 1] = '\0';

  // Copy hash
  strncpy(service.hash, hash, sizeof(service.hash) - 1);
  service.hash[sizeof(service.hash) - 1] = '\0';

  // Copy alias
  strncpy(service.alias, connector->config.alias, sizeof(service.alias) - 1);
  service.alias[sizeof(service.alias) - 1] = '\0';

  connector->config.auto_discovery = false;
  bool failure = false;

  if (ddb_service_reporter_init(&connector->reporter,
                                connector->config.ini_filepath) != 0) {
    fprintf(stderr, "failed to initialize service reporter\n");
    failure = true;
  } else {
    if (ddb_report_service(&connector->reporter, &service) != 0) {
      fprintf(stderr, "failed to report new service\n");
      failure = true;
    } else {
      connector->config.auto_discovery = false;
      if (connector->config.wait_for_attach) {
        ddb_wait_for_debugger();
      } else {
        ddb_setup_signal_handler();
      }
    }
  }

  if (failure) {
    ddb_setup_signal_handler();
  }
}

static inline void ddb_deinit_discovery(DDBConnector *connector) {
  int ret_val = ddb_service_reporter_deinit(&connector->reporter);
  if (ret_val)
    fprintf(stderr, "failed to deinit service reporter\n");
}

static inline void ddb_deinit(DDBConnector *connector) {
  if (connector->config.auto_discovery) {
    ddb_deinit_discovery(connector);
  }
  ddb_unblock_signal(SIGDDBWAIT);
}

static inline void ddb_connector_init(DDBConnector *connector) {
  populate_ddb_metadata(connector->config.ipv4);
  if (connector->config.auto_discovery) {
    ddb_init_discovery(connector);
  } else {
    ddb_setup_signal_handler();
  }
  printf("ddb connector initialized. meta = { pid = %d, comm_ip = %d, ipv4_str "
         "= %s }\n",
         ddb_meta.pid, ddb_meta.comm_ip, ddb_meta.ipv4_str);
}

static inline void ddb_connector_destroy(DDBConnector *connector) {
  if (connector) {
    ddb_deinit(connector);
    free(connector);
  }
}

#ifdef __cplusplus
}
#endif
