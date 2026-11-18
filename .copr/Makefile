# COPR "make srpm" source method (docs/DEVELOPMENT.md › Releases): the spec at the version
# in Cargo.toml, its sources downloaded from GitHub (the tag must be pushed).
version := $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)

srpm:
	dnf -y install rpm-build rpmdevtools
	mkdir -p $(outdir)
	sed 's/^Version: .*/Version:        $(version)/' packaging/rpm/canalis.spec > $(outdir)/canalis.spec
	spectool -g -C $(outdir) $(outdir)/canalis.spec
	rpmbuild -bs --define "_sourcedir $(outdir)" --define "_srcrpmdir $(outdir)" $(outdir)/canalis.spec
