# Sheng installation dependency repair

Baseline: peiyuesheng666/skyward main, 3850dbd.

## Failure and repair

Private service mount namespaces retained logical devices after recovery
unmounted them. Use one-way slave propagation for service-local binds and
release unused logical mappings before parking pinned parent mounts. Report
remaining namespace mounts when unmapping fails.

Service executable paths can change after lazy detach. Match device/inode
identity and pin actual mapped/open handles rather than resolving old names.

Sheng also loads ADSP, health and secure firmware from the current modem.
The early charger fstab leaves that physical block device read-only. Before
those consumers start, copy the complete firmware to RAM, verify every byte,
mount a read-only loop, move firmware aliases, and verify that no namespace
or process retains the original. Only then clear its block read-only flag.
Recheck isolation before allowing an installer to start. Preparation failure
keeps the UI available but blocks installation.

## Verification

- 3 snapshot tests, 36 block and 16 VAB tests; ARM64 release build.
- Namespace regression probe contrasts private mounts and park-before ordering
  with slave propagation and park-after ordering, entirely on disposable tmpfs.
- Independent image comparison: only recovery executable changed among
  903 ramdisk entries.
- 192937984 firmware bytes verified in RAM, read-only loop, original device
  writable and released from all observed process mount namespaces.
- Real GUI installation of a 10.2 GB ROM ZIP completed with script exit 0.
  Firmware writes actually ran; no skip-if-equal workaround.
- Readback matched all 20 physical A/B image ranges and the entire Super.
- Android completed boot; root, user credential storage and launcher worked.

The full-device test rewrote firmware from the same official version. Source
mutation unit tests establish that changing source bytes does not alter the
RAM copy; a cross-version full-device upgrade has not been tested. This hook
is scoped to this port's remoteproc firmware configuration.

Local diagnostics, device identifiers, credentials and recovery images are not
included in this source PR. The local acceptance harness adds a restricted
non-writing probe command; that test-only entry is omitted here.

## Shared follow-up validation

The CLI basename guard and strict device/inode fallback for Android page-size
compatibility handles were subsequently verified on Byron and ported here.
Both production PR branches pass host tests and ARM64 compilation. These
follow-ups have not been reflashed to the Sheng device; the full-ROM evidence
above applies to the rc5 runtime before these two follow-ups.
