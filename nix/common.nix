# Shared NixOS VM config: autologin user "alice" on tty1 that starts Hyprland
# with a Lua config. virtio-gpu gives aquamarine a DRM node; mesa llvmpipe
# renders, so no host GPU/KVM is required (KVM just makes it much faster).
{ pkgs, extraLua ? "", extraPackages ? [ ] }:
{
  users.users.alice = { isNormalUser = true; uid = 1000; password = "alice"; };
  services.getty.autologinUser = "alice";
  programs.hyprland.enable = true;

  environment.systemPackages = [
    pkgs.kitty
    pkgs.jq
    pkgs.git
    pkgs.socat
    # `hc …` = hyprctl against the running instance, usable from `su alice -c`.
    (pkgs.writeShellScriptBin "hc" ''
      export XDG_RUNTIME_DIR=/run/user/1000
      export HYPRLAND_INSTANCE_SIGNATURE=$(ls -t /run/user/1000/hypr | head -1)
      exec hyprctl "$@"
    '')
    # `as-alice CMD…` = run CMD with the session env Hyprland children get.
    (pkgs.writeShellScriptBin "as-alice" ''
      export XDG_RUNTIME_DIR=/run/user/1000
      export HYPRLAND_INSTANCE_SIGNATURE=$(ls -t /run/user/1000/hypr | head -1)
      export WAYLAND_DISPLAY=wayland-1
      exec "$@"
    '')
  ] ++ extraPackages;

  environment.etc."hypr-test.lua".text = ''
    hl.monitor({ output = "", mode = "1280x800@60", position = "0x0", scale = "1" })
    -- keep the emulated (TCG) VM responsive
    hl.config({ animations = { enabled = false }, misc = { disable_hyprland_logo = true, disable_splash_rendering = true } })
    ${extraLua}
  '';

  programs.bash.loginShellInit = ''
    if [ "$(tty)" = "/dev/tty1" ]; then
      mkdir -p ~/.config/hypr
      cp /etc/hypr-test.lua ~/.config/hypr/hyprland.lua
      Hyprland > /tmp/hyprland.out 2>&1
    fi
  '';

  virtualisation.qemu.options = [ "-vga none" "-device virtio-gpu-pci" ];
  virtualisation.memorySize = 2048;
  virtualisation.cores = 4;
}
