{ pkgs ? import <nixpkgs> {} }:

let
  x11 = with pkgs; [ libx11 libxrandr libxinerama libxcursor libxi libxext ];
in pkgs.mkShell {
  packages = with pkgs; [
    cargo rustc clang llvmPackages.libclang cmake pkg-config
    alsa-lib libGL mesa
    glib dbus cairo pango gdk-pixbuf gtk3 luajit xorg-server patchelf
  ] ++ x11;

  CMAKE_INCLUDE_PATH = pkgs.lib.concatStringsSep ":" (map (package: "${package.dev}/include") x11);
  CMAKE_LIBRARY_PATH = pkgs.lib.concatStringsSep ":" (map (package: "${package}/lib") x11);
  LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
  LIBGL_DRIVERS_PATH = "${pkgs.mesa}/lib/dri";
  LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath [ pkgs.libGL pkgs.mesa ];
}
