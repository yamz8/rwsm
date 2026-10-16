# Install rwsm as a drop-in replacement for uwsm.
#
#   make
#   sudo make install            # PREFIX=/usr by default
#   make install DESTDIR=/tmp/stage
#   sudo make install EXTRAS=all # also install the optional uwsm extras
#
# BIN_PATH and LIBEXEC_DIR are compiled into the binary, so pass the same
# PREFIX/LIBEXECDIR to both targets.
#
# The optional extras mirror uwsm's meson options and are off by default, as
# upstream: UWSM_APP, UUCTL, WAIT_TRAY, FUMON, TTYAUTOLOCK (set to 1, or pass
# EXTRAS=all). FUMON_PRESET (enable) and TTYAUTOLOCK_PRESET (disable) set the
# systemd user preset for their units. MAN_PAGES=auto builds man pages when
# scdoc is available (1 requires it, 0 skips them).

PREFIX     ?= /usr
BINDIR     ?= $(PREFIX)/bin
LIBEXECDIR ?= $(PREFIX)/libexec
DATADIR    ?= $(PREFIX)/share
MANDIR     ?= $(DATADIR)/man
DOCDIR     ?= $(DATADIR)/doc/uwsm
LICENSEDIR ?= $(DATADIR)/licenses/rwsm
UNITDIR    ?= $(PREFIX)/lib/systemd/user
PRESETDIR  ?= $(PREFIX)/lib/systemd/user-preset
WAITPID_BIN ?= waitpid
CARGO      ?= cargo
SCDOC      ?= scdoc
MAN_PAGES  ?= auto

EXTRAS ?=
ifeq ($(EXTRAS),all)
UWSM_APP    = 1
UUCTL       = 1
WAIT_TRAY   = 1
FUMON       = 1
TTYAUTOLOCK = 1
endif
UWSM_APP    ?= 0
UUCTL       ?= 0
WAIT_TRAY   ?= 0
FUMON       ?= 0
TTYAUTOLOCK ?= 0
FUMON_PRESET       ?= enable
TTYAUTOLOCK_PRESET ?= disable

BIN_PATH    = $(BINDIR)/uwsm
LIBEXEC_DIR = $(LIBEXECDIR)/uwsm

export UWSM_BIN_PATH = $(BIN_PATH)
export UWSM_LIBEXEC_DIR = $(LIBEXEC_DIR)

MAN_SOURCES = uwsm.1 uwsm-app.1 uwsm-plugins.3 \
	$(if $(filter 1,$(FUMON)),fumon.1) \
	$(if $(filter 1,$(UUCTL)),uuctl.1) \
	$(if $(filter 1,$(TTYAUTOLOCK)),ttyautolock.1)

.PHONY: all build install install-man clean

all: build

build:
	$(CARGO) build --release --locked

install: build install-man
	install -Dm755 target/release/uwsm $(DESTDIR)$(BIN_PATH)
	install -Dm755 -t $(DESTDIR)$(LIBEXEC_DIR) data/libexec/*.sh
	install -d $(DESTDIR)$(DATADIR)/uwsm/plugins
	cp -P data/plugins/*.sh $(DESTDIR)$(DATADIR)/uwsm/plugins/
	install -d $(DESTDIR)$(UNITDIR)
	for f in data/systemd/user/*; do \
		case "$$(basename "$$f")" in \
		fumon.service) [ "$(FUMON)" = 1 ] || continue ;; \
		ttyautolock@.service) [ "$(TTYAUTOLOCK)" = 1 ] || continue ;; \
		esac; \
		case "$$f" in \
		*.in) sed -e 's|@BIN_PATH@|$(BIN_PATH)|g' -e 's|@BIN_NAME@|uwsm|g' \
			-e 's|@WAITPID_BIN@|$(WAITPID_BIN)|g' "$$f" \
			> "$(DESTDIR)$(UNITDIR)/$$(basename "$$f" .in)" ;; \
		*) install -m644 "$$f" $(DESTDIR)$(UNITDIR)/ ;; \
		esac; \
	done
	if [ "$(UWSM_APP)" = 1 ]; then \
		install -Dm755 data/scripts/uwsm-app.sh $(DESTDIR)$(BINDIR)/uwsm-app; \
		for l in uwsm-terminal uwsm-terminal-service uwsm-terminal-scope; do \
			ln -sf uwsm-app $(DESTDIR)$(BINDIR)/$$l; \
		done; \
	fi
	if [ "$(UUCTL)" = 1 ]; then \
		install -Dm755 data/scripts/uuctl.sh $(DESTDIR)$(BINDIR)/uuctl; \
		install -Dm644 data/scripts/uuctl.desktop $(DESTDIR)$(DATADIR)/applications/uuctl.desktop; \
	fi
	if [ "$(WAIT_TRAY)" = 1 ]; then \
		install -Dm755 data/scripts/wait-tray.sh $(DESTDIR)$(BINDIR)/wait-tray; \
	fi
	if [ "$(FUMON)" = 1 ]; then \
		install -Dm755 data/scripts/fumon.sh $(DESTDIR)$(BINDIR)/fumon; \
		install -d $(DESTDIR)$(PRESETDIR); \
		sed -e 's|@BIN_NAME@|uwsm|g' -e 's|@FUMON_PRESET@|$(FUMON_PRESET)|g' \
			data/systemd/user-preset/80-fumon.preset.in > $(DESTDIR)$(PRESETDIR)/80-fumon.preset; \
	fi
	if [ "$(TTYAUTOLOCK)" = 1 ]; then \
		install -Dm755 data/scripts/ttyautolock.sh $(DESTDIR)$(BINDIR)/ttyautolock; \
		install -d $(DESTDIR)$(PRESETDIR); \
		sed -e 's|@BIN_NAME@|uwsm|g' -e 's|@TTYAUTOLOCK_PRESET@|$(TTYAUTOLOCK_PRESET)|g' \
			data/systemd/user-preset/80-ttyautolock.preset.in > $(DESTDIR)$(PRESETDIR)/80-ttyautolock.preset; \
	fi
	install -Dm644 data/README.uwsm.md $(DESTDIR)$(DOCDIR)/README.md
	install -Dm644 -t $(DESTDIR)$(DOCDIR)/example-units data/example-units/*
	install -Dm644 LICENSE $(DESTDIR)$(LICENSEDIR)/LICENSE
	install -Dm644 data/LICENSE.uwsm $(DESTDIR)$(LICENSEDIR)/LICENSE.uwsm

install-man:
	@if [ "$(MAN_PAGES)" = 0 ]; then exit 0; fi; \
	if ! command -v $(SCDOC) >/dev/null; then \
		if [ "$(MAN_PAGES)" = 1 ]; then echo "scdoc not found" >&2; exit 1; fi; \
		echo "scdoc not found, skipping man pages"; exit 0; \
	fi; \
	for m in $(MAN_SOURCES); do \
		section=$${m##*.}; \
		install -d $(DESTDIR)$(MANDIR)/man$$section; \
		$(SCDOC) < data/man/$$m.scd > $(DESTDIR)$(MANDIR)/man$$section/$$m || exit 1; \
	done

clean:
	$(CARGO) clean
