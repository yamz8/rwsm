# Install rwsm as a drop-in replacement for uwsm.
#
#   make
#   sudo make install            # PREFIX=/usr by default
#   make install DESTDIR=/tmp/stage
#
# BIN_PATH and LIBEXEC_DIR are compiled into the binary, so pass the same
# PREFIX/LIBEXECDIR to both targets.

PREFIX     ?= /usr
BINDIR     ?= $(PREFIX)/bin
LIBEXECDIR ?= $(PREFIX)/libexec
DATADIR    ?= $(PREFIX)/share
UNITDIR    ?= $(PREFIX)/lib/systemd/user
WAITPID_BIN ?= waitpid
CARGO      ?= cargo

BIN_PATH    = $(BINDIR)/uwsm
LIBEXEC_DIR = $(LIBEXECDIR)/uwsm

export UWSM_BIN_PATH = $(BIN_PATH)
export UWSM_LIBEXEC_DIR = $(LIBEXEC_DIR)

.PHONY: all build install clean

all: build

build:
	$(CARGO) build --release --locked

install: build
	install -Dm755 target/release/uwsm $(DESTDIR)$(BIN_PATH)
	install -Dm755 -t $(DESTDIR)$(LIBEXEC_DIR) data/libexec/*.sh
	install -d $(DESTDIR)$(DATADIR)/uwsm/plugins
	cp -P data/plugins/*.sh $(DESTDIR)$(DATADIR)/uwsm/plugins/
	install -d $(DESTDIR)$(UNITDIR)
	for f in data/systemd/user/*; do \
		case "$$f" in \
		*.in) sed -e 's|@BIN_PATH@|$(BIN_PATH)|g' -e 's|@BIN_NAME@|uwsm|g' \
			-e 's|@WAITPID_BIN@|$(WAITPID_BIN)|g' "$$f" \
			> "$(DESTDIR)$(UNITDIR)/$$(basename "$$f" .in)" ;; \
		*) install -m644 "$$f" $(DESTDIR)$(UNITDIR)/ ;; \
		esac; \
	done
	install -Dm755 data/scripts/uwsm-app.sh $(DESTDIR)$(BINDIR)/uwsm-app
	for l in uwsm-terminal uwsm-terminal-service uwsm-terminal-scope; do \
		ln -sf uwsm-app $(DESTDIR)$(BINDIR)/$$l; \
	done
	install -Dm755 data/scripts/uuctl.sh $(DESTDIR)$(BINDIR)/uuctl
	install -Dm644 data/scripts/uuctl.desktop $(DESTDIR)$(DATADIR)/applications/uuctl.desktop
	install -Dm755 data/scripts/wait-tray.sh $(DESTDIR)$(BINDIR)/wait-tray
	install -Dm644 data/LICENSE.uwsm $(DESTDIR)$(DATADIR)/licenses/rwsm/LICENSE.uwsm

clean:
	$(CARGO) clean
