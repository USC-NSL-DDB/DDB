NCORES = $(shell nproc)

# gdb preparation/installation for included gdb-14.2
gdb-clean:
	(cd gdb-14.2/build && make clean) > /dev/null 2>&1 || true
	rm -rf gdb-14.2/build > /dev/null 2>&1 || true

gdb: gdb-clean
	pushd gdb-14.2 && \
	mkdir -p build && pushd build && \
	../configure --disable-install-man --with-python=/usr/bin/python3 && make -j$(NCORES) && \
	popd && popd

gdb-install: gdb
	pushd gdb-14.2/build && sudo make install

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
