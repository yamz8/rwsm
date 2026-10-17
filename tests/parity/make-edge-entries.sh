#!/bin/bash
# usage: make-edge-entries.sh DIR -> writes edge-N.desktop files covering
# pyxdg's parser and validator rules that decide whether uwsm accepts an entry.
# Each prints its own ID list on stdout, plus the action IDs to try.
set -e
A=$1; mkdir -p "$A"; i=0
mk() { i=$((i+1)); printf '%b' "$1" > "$A/edge-$i.desktop"; echo "edge-$i.desktop"; }
H='[Desktop Entry]\nType=Application\nName=E\n'
mk "${H}Exec=fakecomp a\nExec=fakecomp b\n"                       # duplicate key: last wins
mk "${H}Exec=fakecomp\n[Desktop Entry]\nType=Application\nName=F\nExec=fakecomp second\n" # duplicate group resets it
mk "${H}Exec=fakecomp\nExec[de]=fakecomp de\n"                    # locale on a non-locale key (tolerated)
mk "${H}Exec=fakecomp\nName[de_DE.UTF-8@euro]=x\nComment[sr@latin]=y\n"
mk "${H}Exec=fakecomp\nName[bad-loc]=x\n"
mk "${H}Exec=fakecomp ñ\n"                                         # non-ASCII string value
mk "${H}Exec=fakecomp\nIcon=ícono\n"
mk "${H}Exec=fakecomp\nTerminal=1\n"                              # deprecated boolean, only a warning
mk "${H}Exec=fakecomp\nTerminal=yes\n"
mk "${H}Exec=fakecomp\nImplements=org.foo\n"                      # unknown to pyxdg
mk "${H}Exec=fakecomp\nURL=http://x\n"                            # pyxdg restricts URL to Type=URL
mk "[Desktop Entry]\nType=Link\nName=L\nURL=http://x\nExec=fakecomp\n"
mk "[Desktop Entry]\nType=Bogus\nName=B\nExec=fakecomp\n"
mk "${H}Exec=fakecomp\nOnlyShowIn=GNOME;\nNotShowIn=KDE;\n"
mk "${H}Exec=fakecomp\n[Bad Group]\nName=x\n"
mk "${H}Exec=fakecomp\n[X-ñ]\nName=x\n"
mk "${H}Exec=fakecomp --main\nActions=a;b;\n[Desktop Action a]\nName=A\nExec=fakecomp --a\n" # listed action without a group
mk "${H}Exec=fakecomp\nInitialPreference=abc\n"
mk "${H}Exec=fakecomp\nInitialPreference=1_0.5e3\n"
mk "${H}Exec=fakecomp\nComment=caf\\xe9\n"                        # not UTF-8
mk "${H}Exec=fakecomp\nX-=1\nX-_foo=2\n"
mk "${H}Exec  =  fakecomp spaced  \n"
mk "${H}Exec=fakecomp\nMimeType=text/plain;tèxt/x;\n"
mk "${H}Exec=fakecomp\nCategories=Foo;Bar;\n"                     # unregistered category (tolerated)
mk "${H}Exec=fakecomp\nOnlyShowIn=Hyprland;\n"                    # unregistered desktop (tolerated)
mk "${H}Exec=fakecomp\nnotakeyline\n"                             # parse errors
mk "Exec=fakecomp\n${H}"
mk "[Other]\nName=x\n"
mk "${H}Exec=fakecomp it's\n"                                     # Exec tokenizer errors
mk "${H}Exec=fakecomp \$HOME\n"
mk "${H}Exec=fakecomp\nHidden=true\n"
mk "${H}Exec=fakecomp\nTryExec=nonexistent-xyz\n"
mk "${H}Exec=fakecomp\nKeywords[fr]=a;b;\n"
mk "${H}Exec=\n"
mk "[Desktop Entry]\nExec=fakecomp\n"
mk "${H}Exec=fakecomp\nFoo-bar[x]=1\n"                            # not matched by uwsm's locale-key filter
mk "[Desktop Entry]\nType=Link\nName=L\nURL=x\nStartupWMClass=x\n"
mk "${H}Exec=fakecomp\nVersion=1.5\nVersion=ü\n"
mk "${H}Exec=fakecomp\nOnlyShowIn=a|b|ü\n"                        # pyxdg list separators
mk "${H}Exec=fakecomp\nOnlyShowIn=a\\\;b;ü,c\n"
mk "${H}Exec=fakecomp\n[Desktop Action a]\nName=A\nExec=fakecomp\nBogusKey=1\n"
mk "${H}Exec=fakecomp\n[X-Foo]\nTargetEnvironment=x\nOther=y\n"
mk "${H}Exec=fakecomp\nInitialPreference= 7 \n"
mk "${H}Exec=fakecomp\n  # comment\n\n"
mk "${H}Exec=fakecomp\nType=Application\nName[x]=1\nName[]=2\n"
echo edge-17.desktop:a edge-17.desktop:b edge-17.desktop:c
