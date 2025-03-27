PREFIX ?= $(HOME)/.local
INCLUDEDIR := $(PREFIX)/include

CEREAL_HDRS := $(shell find include/cereal -type f \( -name "*.h" -o -name "*.hpp" \))
CCDDB_HDRS := $(wildcard include/cddb/*.h)
CXXDDB_HDRS := $(wildcard include/ddb/*.hpp)

.PHONY: install uninstall install-cereal install-ccddb install-cxxddb
	
install-cereal:
	@echo "Installing cereal headers..."
	@for hdr in $(CEREAL_HDRS); do \
		rel=$$(echo $$hdr | sed 's|^include/||'); \
		install -Dm644 $$hdr $(INCLUDEDIR)/$$rel; \
	done

install-ccddb:
	@echo "Installing cddb headers..."
	@for hdr in $(CCDDB_HDRS); do \
		rel=$$(echo $$hdr | sed 's|^include/||'); \
		install -Dm644 $$hdr $(INCLUDEDIR)/$$rel; \
	done

install-cxxddb:
	@echo "Installing cxxddb headers..."
	@for hdr in $(CXXDDB_HDRS); do \
		rel=$$(echo $$hdr | sed 's|^include/||'); \
		install -Dm644 $$hdr $(INCLUDEDIR)/$$rel; \
	done	

install:
	@echo "Installing headers to $(INCLUDEDIR)"
	$(MAKE) install-cereal
	$(MAKE) install-ccddb
	$(MAKE) install-cxxddb

# skip cereal cleanup in case other apps use it
uninstall:
	@echo "Uninstalling headers from $(INCLUDEDIR)"
	rm -rf $(INCLUDEDIR)/ddb
	rm -rf $(INCLUDEDIR)/cddb
