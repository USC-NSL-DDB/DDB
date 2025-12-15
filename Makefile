NCORES = $(shell nproc)

install-gdb:
	cd ./scripts && ./install_gdb.sh

.PHONY: install-connector
install-connector:
	$(MAKE) -C connector install

.PHONY: install-broker
install-broker:
	cd ./scripts && ./setup_mqtt.sh

.PHONY: gdb-config-setup
gdb-config-setup:
	cd ./scripts && ./setup_gdb.sh

.PHONY: setup
setup:
	cd ./scripts && ./setup.sh

.PHONY: rpc-framework-setup
rpc-framework-setup: install-broker
	cd ./scripts && ./rpc_framework_setup.sh
