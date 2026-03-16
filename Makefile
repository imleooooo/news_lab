.PHONY: build release icon app installer clean

build:
	cargo build

release:
	cargo build --release

icon:
	python3 scripts/make_icon.py

app: release
	bash scripts/build_app.sh

installer: release
	bash scripts/make_installer.sh

clean:
	cargo clean
	rm -rf ~/Applications/News\ Lab.app
	rm -f "News Lab"-*.pkg
	rm -f resources/icon.icns
