{
  description = "Lintel, a native Wayland desktop shell";
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/f9a75f41be3a74eab55d3a7679d34ad6cc0da977";

  outputs = {nixpkgs, ...}: let
    system = "x86_64-linux";
    pkgs = import nixpkgs {inherit system;};
    lintel = pkgs.callPackage ./nix/shell.nix {};
  in {
    nixosModules.default = ./nix/module.nix;
    packages.${system} = {
      inherit lintel;
      default = lintel;
    };
  };
}
