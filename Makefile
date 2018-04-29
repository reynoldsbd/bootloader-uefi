arch ?= x86_64
ovmf ?= /usr/share/ovmf/OVMF.fd
profile ?= debug

build_dir := target/$(arch)

kernel := kernel/target/$(arch)-rust_os/$(profile)/kernel
bootloader := bootloader-uefi/target/$(arch)-pc-uefi/$(profile)/bootloader-uefi.efi
esp := $(build_dir)/EFISys.img
iso := $(build_dir)/rust_os.iso


export RUST_TARGET_PATH=$(abspath .)
ifeq ($(profile), debug)
	profile_arg :=
else
	profile_arg := --$(profile)
endif


all: kernel bootloader


kernel: $(kernel)


bootloader: $(bootloader)


test: $(iso)
	@qemu-system-$(arch) -net none -bios $(ovmf) -cdrom $(iso)


debug: $(iso)
	@qemu-system-$(arch) -net none -bios $(ovmf) -cdrom $(iso) -s -S


clean:
	@rm -rf $(build_dir)
	@cd bootloader-uefi; xargo clean
	@cd kernel; xargo clean


$(kernel): $(shell find kernel/src -type f)
	@cd kernel; \
		xargo build \
			--target=$(arch)-rust_os
			$(profile_arg)


$(bootloader): $(shell find bootloader-uefi/src -type f)
	@cd bootloader-uefi; \
		xargo build \
			--target=$(arch)-pc-uefi \
			$(profile_arg)


$(esp): $(kernel) $(bootloader)
	@mkdir -p $(build_dir)/esp/EFI/BOOT
	@mkdir -p $(build_dir)/esp/EFI/RustOs
	@cp $(bootloader) $(build_dir)/esp/EFI/BOOT/BOOTX64.EFI
	@cp $(kernel) $(build_dir)/esp/EFI/RustOs/Kernel
	@rm -f $(esp)
	@dd if=/dev/zero of=$(esp) bs=1M count=64
	@mkfs.vfat -F 32 $(esp) -n EFISys
	@mcopy -i $(esp) -s $(build_dir)/esp/EFI ::


$(iso): $(esp)
	@mkdir -p $(build_dir)/iso
	@cp $(esp) $(build_dir)/iso/
	@xorriso -as mkisofs \
		-o $(iso) \
		-e $(notdir $(esp)) \
		-no-emul-boot \
		$(build_dir)/iso
