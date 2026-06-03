use arbitrary::Arbitrary;

/// Represents a individual programming language. Can be used to provide
/// information about the language, such as multi line comments, single line
/// comments, string literal syntax, whether a given language allows nesting
/// comments.
#[derive(Deserialize)]
#[derive(Arbitrary, Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[non_exhaustive]
#[allow(clippy::upper_case_acronyms)]
pub enum LanguageType {
    #[allow(missing_docs)]  #[serde(alias = "ABAP")]  Abap,
    #[allow(missing_docs)]  #[serde(alias = "ABNF")]  ABNF,
    #[allow(missing_docs)]  #[serde(alias = "ActionScript")]  ActionScript,
    #[allow(missing_docs)]  #[serde(alias = "Ada")]  Ada,
    #[allow(missing_docs)]  #[serde(alias = "Agda")]  Agda,
    #[allow(missing_docs)]  #[serde(alias = "Alex")]  Alex,
    #[allow(missing_docs)]  #[serde(alias = "Alloy")]  Alloy,
    #[allow(missing_docs)]  #[serde(alias = "APL")]  Apl,
    #[allow(missing_docs)]  #[serde(alias = "Arduino C++")]  Arduino,
    #[allow(missing_docs)]  #[serde(alias = "Ark TypeScript")]  ArkTS,
    #[allow(missing_docs)]  #[serde(alias = "Arturo")]  Arturo,
    #[allow(missing_docs)]  #[serde(alias = "AsciiDoc")]  AsciiDoc,
    #[allow(missing_docs)]  #[serde(alias = "ASN.1")]  Asn1,
    #[allow(missing_docs)]  #[serde(alias = "ASP")]  Asp,
    #[allow(missing_docs)]  #[serde(alias = "ASP.NET")]  AspNet,
    #[allow(missing_docs)]  #[serde(alias = "Assembly")]  Assembly,
    #[allow(missing_docs)]  #[serde(alias = "GNU Style Assembly")]  AssemblyGAS,
    #[allow(missing_docs)]  #[serde(alias = "Astro")]  Astro,
    #[allow(missing_docs)]  #[serde(alias = "ATS")]  Ats,
    #[allow(missing_docs)]  #[serde(alias = "Autoconf")]  Autoconf,
    #[allow(missing_docs)]  #[serde(alias = "Autoit")]  Autoit,
    #[allow(missing_docs)]  #[serde(alias = "AutoHotKey")]  AutoHotKey,
    #[allow(missing_docs)]  #[serde(alias = "Automake")]  Automake,
    #[allow(missing_docs)]  #[serde(alias = "AXAML")]  AvaloniaXaml,
    #[allow(missing_docs)]  #[serde(alias = "AWK")]  AWK,
    #[allow(missing_docs)]  #[serde(alias = "Ballerina")]  Ballerina,
    #[allow(missing_docs)]  #[serde(alias = "BASH")]  Bash,
    #[allow(missing_docs)]  #[serde(alias = "Batch")]  Batch,
    #[allow(missing_docs)]  #[serde(alias = "Bazel")]  Bazel,
    #[allow(missing_docs)]  #[serde(alias = "Bean")]  Bean,
    #[allow(missing_docs)]  #[serde(alias = "Bicep")]  Bicep,
    #[allow(missing_docs)]  #[serde(alias = "Bitbake")]  Bitbake,
    #[allow(missing_docs)]  #[serde(alias = "BQN")]  Bqn,
    #[allow(missing_docs)]  #[serde(alias = "BrightScript")]  BrightScript,
    #[allow(missing_docs)]  #[serde(alias = "C")]  C,
    #[allow(missing_docs)]  #[serde(alias = "Cabal")]  Cabal,
    #[allow(missing_docs)]  #[serde(alias = "Cairo")]  Cairo,
    #[allow(missing_docs)]  #[serde(alias = "Cangjie")]  Cangjie,
    #[allow(missing_docs)]  #[serde(alias = "Cassius")]  Cassius,
    #[allow(missing_docs)]  #[serde(alias = "Ceylon")]  Ceylon,
    #[allow(missing_docs)]  #[serde(alias = "Chapel")]  Chapel,
    #[allow(missing_docs)]  #[serde(alias = "C Header")]  CHeader,
    #[allow(missing_docs)]  #[serde(alias = "CIL (SELinux)")]  Cil,
    #[allow(missing_docs)]  #[serde(alias = "Circom")]  Circom,
    #[allow(missing_docs)]  #[serde(alias = "Clojure")]  Clojure,
    #[allow(missing_docs)]  #[serde(alias = "ClojureC")]  ClojureC,
    #[allow(missing_docs)]  #[serde(alias = "ClojureScript")]  ClojureScript,
    #[allow(missing_docs)]  #[serde(alias = "CMake")]  CMake,
    #[allow(missing_docs)]  #[serde(alias = "COBOL")]  Cobol,
    #[allow(missing_docs)]  #[serde(alias = "CodeQL")]  CodeQL,
    #[allow(missing_docs)]  #[serde(alias = "CoffeeScript")]  CoffeeScript,
    #[allow(missing_docs)]  #[serde(alias = "Cogent")]  Cogent,
    #[allow(missing_docs)]  #[serde(alias = "ColdFusion")]  ColdFusion,
    #[allow(missing_docs)]  #[serde(alias = "ColdFusion CFScript")]  ColdFusionScript,
    #[allow(missing_docs)]  #[serde(alias = "Coq")]  Coq,
    #[allow(missing_docs)]  #[serde(alias = "C++")]  Cpp,
    #[allow(missing_docs)]  #[serde(alias = "C++ Header")]  CppHeader,
    #[allow(missing_docs)]  #[serde(alias = "C++ Module")]  CppModule,
    #[allow(missing_docs)]  #[serde(alias = "Crystal")]  Crystal,
    #[allow(missing_docs)]  #[serde(alias = "C#")]  CSharp,
    #[allow(missing_docs)]  #[serde(alias = "C Shell")]  CShell,
    #[allow(missing_docs)]  #[serde(alias = "CSS")]  Css,
    #[allow(missing_docs)]  #[serde(alias = "CUDA")]  Cuda,
    #[allow(missing_docs)]  #[serde(alias = "CUE")]  Cue,
    #[allow(missing_docs)]  #[serde(alias = "Cython")]  Cython,
    #[allow(missing_docs)]  #[serde(alias = "D")]  D,
    #[allow(missing_docs)]  #[serde(alias = "D2")]  D2,
    #[allow(missing_docs)]  #[serde(alias = "DAML")]  Daml,
    #[allow(missing_docs)]  #[serde(alias = "Dart")]  Dart,
    #[allow(missing_docs)]  #[serde(alias = "Device Tree")]  DeviceTree,
    #[allow(missing_docs)]  #[serde(alias = "Dhall")]  Dhall,
    #[allow(missing_docs)]  #[serde(alias = "Dockerfile")]  Dockerfile,
    #[allow(missing_docs)]  #[serde(alias = ".NET Resource")]  DotNetResource,
    #[allow(missing_docs)]  #[serde(alias = "Dream Maker")]  DreamMaker,
    #[allow(missing_docs)]  #[serde(alias = "Dust.js")]  Dust,
    #[allow(missing_docs)]  #[serde(alias = "Ebuild")]  Ebuild,
    #[allow(missing_docs)]  #[serde(alias = "EdgeQL")]  EdgeQL,
    #[allow(missing_docs)]  #[serde(alias = "EdgeDB Schema Definition")]  ESDL,
    #[allow(missing_docs)]  #[serde(alias = "Edn")]  Edn,
    #[allow(missing_docs)]  #[serde(alias = "8th")]  Eighth,
    #[allow(missing_docs)]  #[serde(alias = "Emacs Lisp")]  Elisp,
    #[allow(missing_docs)]  #[serde(alias = "Elixir")]  Elixir,
    #[allow(missing_docs)]  #[serde(alias = "Elm")]  Elm,
    #[allow(missing_docs)]  #[serde(alias = "Elvish")]  Elvish,
    #[allow(missing_docs)]  #[serde(alias = "Emacs Dev Env")]  EmacsDevEnv,
    #[allow(missing_docs)]  #[serde(alias = "Emojicode")]  Emojicode,
    #[allow(missing_docs)]  #[serde(alias = "Erlang")]  Erlang,
    #[allow(missing_docs)]  #[serde(alias = "Factor")]  Factor,
    #[allow(missing_docs)]  #[serde(alias = "FEN")]  FEN,
    #[allow(missing_docs)]  #[serde(alias = "Fennel")]  Fennel,
    #[allow(missing_docs)]  #[serde(alias = "Fish")]  Fish,
    #[allow(missing_docs)]  #[serde(alias = "FlatBuffers Schema")]  FlatBuffers,
    #[allow(missing_docs)]  #[serde(alias = "Forge Config")]  ForgeConfig,
    #[allow(missing_docs)]  #[serde(alias = "Forth")]  Forth,
    #[allow(missing_docs)]  #[serde(alias = "FORTRAN Legacy")]  FortranLegacy,
    #[allow(missing_docs)]  #[serde(alias = "FORTRAN Modern")]  FortranModern,
    #[allow(missing_docs)]  #[serde(alias = "FreeMarker")]  FreeMarker,
    #[allow(missing_docs)]  #[serde(alias = "F#")]  FSharp,
    #[allow(missing_docs)]  #[serde(alias = "F*")]  Fstar,
    #[allow(missing_docs)]  #[serde(alias = "Futhark")]  Futhark,
    #[allow(missing_docs)]  #[serde(alias = "GDB Script")]  GDB,
    #[allow(missing_docs)]  #[serde(alias = "GDScript")]  GdScript,
    #[allow(missing_docs)]  #[serde(alias = "Gherkin (Cucumber)")]  Gherkin,
    #[allow(missing_docs)]  #[serde(alias = "Gleam")]  Gleam,
    #[allow(missing_docs)]  #[serde(alias = "Glimmer JS")]  GlimmerJs,
    #[allow(missing_docs)]  #[serde(alias = "Glimmer TS")]  GlimmerTs,
    #[allow(missing_docs)]  #[serde(alias = "GLSL")]  Glsl,
    #[allow(missing_docs)]  #[serde(alias = "Gml")]  Gml,
    #[allow(missing_docs)]  #[serde(alias = "Go")]  Go,
    #[allow(missing_docs)]  #[serde(alias = "Go HTML")]  Gohtml,
    #[allow(missing_docs)]  #[serde(alias = "GraphQL")]  Graphql,
    #[allow(missing_docs)]  #[serde(alias = "Groovy")]  Groovy,
    #[allow(missing_docs)]  #[serde(alias = "Gwion")]  Gwion,
    #[allow(missing_docs)]  #[serde(alias = "Haml")]  Haml,
    #[allow(missing_docs)]  #[serde(alias = "Hamlet")]  Hamlet,
    #[allow(missing_docs)]  #[serde(alias = "Happy")]  Happy,
    #[allow(missing_docs)]  #[serde(alias = "Handlebars")]  Handlebars,
    #[allow(missing_docs)]  #[serde(alias = "Haskell")]  Haskell,
    #[allow(missing_docs)]  #[serde(alias = "Haxe")]  Haxe,
    #[allow(missing_docs)]  #[serde(alias = "HCL")]  Hcl,
    #[allow(missing_docs)]  #[serde(alias = "Headache")]  Headache,
    #[allow(missing_docs)]  #[serde(alias = "HEX")]  Hex,
    #[allow(missing_docs)]  #[serde(alias = "Hex0")]  Hex0,
    #[allow(missing_docs)]  #[serde(alias = "Hex1")]  Hex1,
    #[allow(missing_docs)]  #[serde(alias = "Hex2")]  Hex2,
    #[allow(missing_docs)]  #[serde(alias = "HICAD")]  HiCad,
    #[allow(missing_docs)]  #[serde(alias = "HLSL")]  Hlsl,
    #[allow(missing_docs)]  #[serde(alias = "HolyC")]  HolyC,
    #[allow(missing_docs)]  #[serde(alias = "HTML")]  Html,
    #[allow(missing_docs)]  #[serde(alias = "Hy")]  Hy,
    #[allow(missing_docs)]  #[serde(alias = "Idris")]  Idris,
    #[allow(missing_docs)]  #[serde(alias = "INI")]  Ini,
    #[allow(missing_docs)]  #[serde(alias = "Intel HEX")]  IntelHex,
    #[allow(missing_docs)]  #[serde(alias = "Isabelle")]  Isabelle,
    #[allow(missing_docs)]  #[serde(alias = "JAI")]  Jai,
    #[allow(missing_docs)]  #[serde(alias = "Janet")]  Janet,
    #[allow(missing_docs)]  #[serde(alias = "Java")]  Java,
    #[allow(missing_docs)]  #[serde(alias = "JavaScript")]  JavaScript,
    #[allow(missing_docs)]  #[serde(alias = "Jinja2")]  Jinja2,
    #[allow(missing_docs)]  #[serde(alias = "jq")]  Jq,
    #[allow(missing_docs)]  #[serde(alias = "JSLT")]  JSLT,
    #[allow(missing_docs)]  #[serde(alias = "JSON")]  Json,
    #[allow(missing_docs)]  #[serde(alias = "Jsonnet")]  Jsonnet,
    #[allow(missing_docs)]  #[serde(alias = "JSX")]  Jsx,
    #[allow(missing_docs)]  #[serde(alias = "Julia")]  Julia,
    #[allow(missing_docs)]  #[serde(alias = "Julius")]  Julius,
    #[allow(missing_docs)]  #[serde(alias = "Jupyter Notebooks")]  Jupyter,
    #[allow(missing_docs)]  #[serde(alias = "Just")]  Just,
    #[allow(missing_docs)]  #[serde(alias = "K")]  K,
    #[allow(missing_docs)]  #[serde(alias = "Kakoune script")]  KakouneScript,
    #[allow(missing_docs)]  #[serde(alias = "Kaem")]  Kaem,
    #[allow(missing_docs)]  #[serde(alias = "Koka")]  Koka,
    #[allow(missing_docs)]  #[serde(alias = "Kotlin")]  Kotlin,
    #[allow(missing_docs)]  #[serde(alias = "Korn shell")]  Ksh,
    #[allow(missing_docs)]  #[serde(alias = "LALRPOP")]  Lalrpop,
    #[allow(missing_docs)]  #[serde(alias = "KV Language")]  KvLanguage,
    #[allow(missing_docs)]  #[serde(alias = "Lean")]  Lean,
    #[allow(missing_docs)]  #[serde(alias = "hledger")]  Hledger,
    #[allow(missing_docs)]  #[serde(alias = "LESS")]  Less,
    #[allow(missing_docs)]  #[serde(alias = "Lex")]  Lex,
    #[allow(missing_docs)]  #[serde(alias = "Liquid")]  Liquid,
    #[allow(missing_docs)]  #[serde(alias = "Lingua Franca")]  LinguaFranca,
    #[allow(missing_docs)]  #[serde(alias = "LD Script")]  LinkerScript,
    #[allow(missing_docs)]  #[serde(alias = "Common Lisp")]  Lisp,
    #[allow(missing_docs)]  #[serde(alias = "LiveScript")]  LiveScript,
    #[allow(missing_docs)]  #[serde(alias = "LLVM")]  LLVM,
    #[allow(missing_docs)]  #[serde(alias = "Logtalk")]  Logtalk,
    #[allow(missing_docs)]  #[serde(alias = "LOLCODE")]  LolCode,
    #[allow(missing_docs)]  #[serde(alias = "Lua")]  Lua,
    #[allow(missing_docs)]  #[serde(alias = "Lucius")]  Lucius,
    #[allow(missing_docs)]  #[serde(alias = "M1 Assembly")]  M1Assembly,
    #[allow(missing_docs)]  #[serde(alias = "M4")]  M4,
    #[allow(missing_docs)]  #[serde(alias = "Madlang")]  Madlang,
    #[allow(missing_docs)]  #[serde(alias = "Makefile")]  Makefile,
    #[allow(missing_docs)]  #[serde(alias = "Markdown")]  Markdown,
    #[allow(missing_docs)]  #[serde(alias = "Max")]  Max,
    #[allow(missing_docs)]  #[serde(alias = "MDX")]  Mdx,
    #[allow(missing_docs)]  #[serde(alias = "Menhir")]  Menhir,
    #[allow(missing_docs)]  #[serde(alias = "Meson")]  Meson,
    #[allow(missing_docs)]  #[serde(alias = "Metal Shading Language")]  Metal,
    #[allow(missing_docs)]  #[serde(alias = "Mint")]  Mint,
    #[allow(missing_docs)]  #[serde(alias = "Mlatu")]  Mlatu,
    #[allow(missing_docs)]  #[serde(alias = "Modelica")]  Modelica,
    #[allow(missing_docs)]  #[serde(alias = "Module-Definition")]  ModuleDef,
    #[allow(missing_docs)]  #[serde(alias = "Mojo")]  Mojo,
    #[allow(missing_docs)]  #[serde(alias = "Monkey C")]  MonkeyC,
    #[allow(missing_docs)]  #[serde(alias = "MoonBit")]  MoonBit,
    #[allow(missing_docs)]  #[serde(alias = "MoonScript")]  MoonScript,
    #[allow(missing_docs)]  #[serde(alias = "MSBuild")]  MsBuild,
    #[allow(missing_docs)]  #[serde(alias = "Mustache")]  Mustache,
    #[allow(missing_docs)]  #[serde(alias = "Nextflow")]  Nextflow,
    #[allow(missing_docs)]  #[serde(alias = "Nim")]  Nim,
    #[allow(missing_docs)]  #[serde(alias = "Nix")]  Nix,
    #[allow(missing_docs)]  #[serde(alias = "Not Quite Perl")]  NotQuitePerl,
    #[allow(missing_docs)]  #[serde(alias = "NuGet Config")]  NuGetConfig,
    #[allow(missing_docs)]  #[serde(alias = "Nushell")]  Nushell,
    #[allow(missing_docs)]  #[serde(alias = "Objective-C")]  ObjectiveC,
    #[allow(missing_docs)]  #[serde(alias = "Objective-C++")]  ObjectiveCpp,
    #[allow(missing_docs)]  #[serde(alias = "OCaml")]  OCaml,
    #[allow(missing_docs)]  #[serde(alias = "Odin")]  Odin,
    #[allow(missing_docs)]  #[serde(alias = "OpenSCAD")]  OpenScad,
    #[allow(missing_docs)]  #[serde(alias = "Open Policy Agent")]  OpenPolicyAgent,
    #[allow(missing_docs)]  #[serde(alias = "OpenCL")]  OpenCL,
    #[allow(missing_docs)]  #[serde(alias = "OpenQASM")]  OpenQasm,
    #[allow(missing_docs)]  #[serde(alias = "OpenType Feature File")]  OpenType,
    #[allow(missing_docs)]  #[serde(alias = "Org")]  Org,
    #[allow(missing_docs)]  #[serde(alias = "Oz")]  Oz,
    #[allow(missing_docs)]  #[serde(alias = "Pacman's makepkg")]  PacmanMakepkg,
    #[allow(missing_docs)]  #[serde(alias = "Pan")]  Pan,
    #[allow(missing_docs)]  #[serde(alias = "Pascal")]  Pascal,
    #[allow(missing_docs)]  #[serde(alias = "Perl")]  Perl,
    #[allow(missing_docs)]  #[serde(alias = "Pest")]  Pest,
    #[allow(missing_docs)]  #[serde(alias = "Phix")]  Phix,
    #[allow(missing_docs)]  #[serde(alias = "PHP")]  Php,
    #[allow(missing_docs)]  #[serde(alias = "PlantUML")]  PlantUml,
    #[allow(missing_docs)]  #[serde(alias = "PO File")]  Po,
    #[allow(missing_docs)]  #[serde(alias = "Poke")]  Poke,
    #[allow(missing_docs)]  #[serde(alias = "Polly")]  Polly,
    #[allow(missing_docs)]  #[serde(alias = "Pony")]  Pony,
    #[allow(missing_docs)]  #[serde(alias = "PostCSS")]  PostCss,
    #[allow(missing_docs)]  #[serde(alias = "PowerShell")]  PowerShell,
    #[allow(missing_docs)]  #[serde(alias = "Lauterbach PRACTICE Script")]  PRACTICE,
    #[allow(missing_docs)]  #[serde(alias = "Processing")]  Processing,
    #[allow(missing_docs)]  #[serde(alias = "Prolog")]  Prolog,
    #[allow(missing_docs)]  #[serde(alias = "PSL Assertion")]  PSL,
    #[allow(missing_docs)]  #[serde(alias = "Protocol Buffers")]  Protobuf,
    #[allow(missing_docs)]  #[serde(alias = "Pug")]  Pug,
    #[allow(missing_docs)]  #[serde(alias = "Puppet")]  Puppet,
    #[allow(missing_docs)]  #[serde(alias = "PureScript")]  PureScript,
    #[allow(missing_docs)]  #[serde(alias = "Pyret")]  Pyret,
    #[allow(missing_docs)]  #[serde(alias = "Python")]  Python,
    #[allow(missing_docs)]  #[serde(alias = "PRQL")]  PRQL,
    #[allow(missing_docs)]  #[serde(alias = "Q")]  Q,
    #[allow(missing_docs)]  #[serde(alias = "QCL")]  Qcl,
    #[allow(missing_docs)]  #[serde(alias = "QML")]  Qml,
    #[allow(missing_docs)]  #[serde(alias = "R")]  R,
    #[allow(missing_docs)]  #[serde(alias = "Racket")]  Racket,
    #[allow(missing_docs)]  #[serde(alias = "Rakefile")]  Rakefile,
    #[allow(missing_docs)]  #[serde(alias = "Raku")]  Raku,
    #[allow(missing_docs)]  #[serde(alias = "Razor")]  Razor,
    #[allow(missing_docs)]  #[serde(alias = "Redscript")]  Redscript,
    #[allow(missing_docs)]  #[serde(alias = "Ren'Py")]  Renpy,
    #[allow(missing_docs)]  #[serde(alias = "ReScript")]  ReScript,
    #[allow(missing_docs)]  #[serde(alias = "ReStructuredText")]  ReStructuredText,
    #[allow(missing_docs)]  #[serde(alias = "Roc")]  Roc,
    #[allow(missing_docs)]  #[serde(alias = "Rusty Object Notation")]  RON,
    #[allow(missing_docs)]  #[serde(alias = "RPM Specfile")]  RPMSpecfile,
    #[allow(missing_docs)]  #[serde(alias = "Ruby")]  Ruby,
    #[allow(missing_docs)]  #[serde(alias = "Ruby HTML")]  RubyHtml,
    #[allow(missing_docs)]  #[serde(alias = "Rust")]  Rust,
    #[allow(missing_docs)]  #[serde(alias = "Sass")]  Sass,
    #[allow(missing_docs)]  #[serde(alias = "Scala")]  Scala,
    #[allow(missing_docs)]  #[serde(alias = "Scheme")]  Scheme,
    #[allow(missing_docs)]  #[serde(alias = "Scons")]  Scons,
    #[allow(missing_docs)]  #[serde(alias = "Shell")]  Sh,
    #[allow(missing_docs)]  #[serde(alias = "ShaderLab")]  ShaderLab,
    #[allow(missing_docs)]  #[serde(alias = "SIL")]  SIL,
    #[allow(missing_docs)]  #[serde(alias = "Slang")]  Slang,
    #[allow(missing_docs)]  #[serde(alias = "Standard ML (SML)")]  Sml,
    #[allow(missing_docs)]  #[serde(alias = "Smalltalk")]  Smalltalk,
    #[allow(missing_docs)]  #[serde(alias = "Snakemake")]  Snakemake,
    #[allow(missing_docs)]  #[serde(alias = "Solidity")]  Solidity,
    #[allow(missing_docs)]  #[serde(alias = "Specman e")]  SpecmanE,
    #[allow(missing_docs)]  #[serde(alias = "Spice Netlist")]  Spice,
    #[allow(missing_docs)]  #[serde(alias = "SQL")]  Sql,
    #[allow(missing_docs)]  #[serde(alias = "SQF")]  Sqf,
    #[allow(missing_docs)]  #[serde(alias = "SRecode Template")]  SRecode,
    #[allow(missing_docs)]  #[serde(alias = "Stan")]  Stan,
    #[allow(missing_docs)]  #[serde(alias = "Stata")]  Stata,
    #[allow(missing_docs)]  #[serde(alias = "Stratego/XT")]  Stratego,
    #[allow(missing_docs)]  #[serde(alias = "Stylus")]  Stylus,
    #[allow(missing_docs)]  #[serde(alias = "Svelte")]  Svelte,
    #[allow(missing_docs)]  #[serde(alias = "SVG")]  Svg,
    #[allow(missing_docs)]  #[serde(alias = "Swift")]  Swift,
    #[allow(missing_docs)]  #[serde(alias = "SWIG")]  Swig,
    #[allow(missing_docs)]  #[serde(alias = "SystemVerilog")]  SystemVerilog,
    #[allow(missing_docs)]  #[serde(alias = "Slint")]  Slint,
    #[allow(missing_docs)]  #[serde(alias = "Tact")]  Tact,
    #[allow(missing_docs)]  #[serde(alias = "TCL")]  Tcl,
    #[allow(missing_docs)]  #[serde(alias = "Tera")]  Tera,
    #[allow(missing_docs)]  #[serde(alias = "Templ")]  Templ,
    #[allow(missing_docs)]  #[serde(alias = "TeX")]  Tex,
    #[allow(missing_docs)]  #[serde(alias = "Plain Text")]  Text,
    #[allow(missing_docs)]  #[serde(alias = "Thrift")]  Thrift,
    #[allow(missing_docs)]  #[serde(alias = "TOML")]  Toml,
    #[allow(missing_docs)]  #[serde(alias = "TSX")]  Tsx,
    #[allow(missing_docs)]  #[serde(alias = "TTCN-3")]  Ttcn,
    #[allow(missing_docs)]  #[serde(alias = "Twig")]  Twig,
    #[allow(missing_docs)]  #[serde(alias = "TypeScript")]  TypeScript,
    #[allow(missing_docs)]  #[serde(alias = "Typst")]  Typst,
    #[allow(missing_docs)]  #[serde(alias = "Uiua")]  Uiua,
    #[allow(missing_docs)]  #[serde(alias = "UMPL")]  UMPL,
    #[allow(missing_docs)]  #[serde(alias = "Unison")]  Unison,
    #[allow(missing_docs)]  #[serde(alias = "Unreal Markdown")]  UnrealDeveloperMarkdown,
    #[allow(missing_docs)]  #[serde(alias = "Unreal Plugin")]  UnrealPlugin,
    #[allow(missing_docs)]  #[serde(alias = "Unreal Project")]  UnrealProject,
    #[allow(missing_docs)]  #[serde(alias = "Unreal Script")]  UnrealScript,
    #[allow(missing_docs)]  #[serde(alias = "Unreal Shader")]  UnrealShader,
    #[allow(missing_docs)]  #[serde(alias = "Unreal Shader Header")]  UnrealShaderHeader,
    #[allow(missing_docs)]  #[serde(alias = "Ur/Web")]  UrWeb,
    #[allow(missing_docs)]  #[serde(alias = "Ur/Web Project")]  UrWebProject,
    #[allow(missing_docs)]  #[serde(alias = "Vala")]  Vala,
    #[allow(missing_docs)]  #[serde(alias = "VB6/VBA")]  VB6,
    #[allow(missing_docs)]  #[serde(alias = "VBScript")]  VBScript,
    #[allow(missing_docs)]  #[serde(alias = "Apache Velocity")]  Velocity,
    #[allow(missing_docs)]  #[serde(alias = "Verilog")]  Verilog,
    #[allow(missing_docs)]  #[serde(alias = "Verilog Args File")]  VerilogArgsFile,
    #[allow(missing_docs)]  #[serde(alias = "VHDL")]  Vhdl,
    #[allow(missing_docs)]  #[serde(alias = "Virgil")]  Virgil,
    #[allow(missing_docs)]  #[serde(alias = "Visual Basic")]  VisualBasic,
    #[allow(missing_docs)]  #[serde(alias = "Visual Studio Project")]  VisualStudioProject,
    #[allow(missing_docs)]  #[serde(alias = "Visual Studio Solution")]  VisualStudioSolution,
    #[allow(missing_docs)]  #[serde(alias = "Vim Script")]  VimScript,
    #[allow(missing_docs)]  #[serde(alias = "Vue")]  Vue,
    #[allow(missing_docs)]  #[serde(alias = "WebAssembly")]  WebAssembly,
    #[allow(missing_docs)]  #[serde(alias = "The WenYan Programming Language")]  WenYan,
    #[allow(missing_docs)]  #[serde(alias = "WebGPU Shader Language")]  WGSL,
    #[allow(missing_docs)]  #[serde(alias = "Wolfram")]  Wolfram,
    #[allow(missing_docs)]  #[serde(alias = "XAML")]  Xaml,
    #[allow(missing_docs)]  #[serde(alias = "Xcode Config")]  XcodeConfig,
    #[allow(missing_docs)]  #[serde(alias = "XML")]  Xml,
    #[allow(missing_docs)]  #[serde(alias = "XSL")]  XSL,
    #[allow(missing_docs)]  #[serde(alias = "Xtend")]  Xtend,
    #[allow(missing_docs)]  #[serde(alias = "YAML")]  Yaml,
    #[allow(missing_docs)]  #[serde(alias = "ZenCode")]  ZenCode,
    #[allow(missing_docs)]  #[serde(alias = "Zig")]  Zig,
    #[allow(missing_docs)]  #[serde(alias = "ZoKrates")]  Zokrates,
    #[allow(missing_docs)]  #[serde(alias = "Zsh")]  Zsh,
    #[allow(missing_docs)]  #[serde(alias = "GDShader")]  GdShader,
    
}

impl LanguageType {

    /// Returns the display name of a language.
    ///
    /// ```
    /// # use tokei::*;
    /// let bash = LanguageType::Bash;
    ///
    /// assert_eq!(bash.name(), "BASH");
    /// ```
    pub fn name(self) -> &'static str {
        match self {
            Abap => "ABAP",
            ABNF => "ABNF",
            ActionScript => "ActionScript",
            Ada => "Ada",
            Agda => "Agda",
            Alex => "Alex",
            Alloy => "Alloy",
            Apl => "APL",
            Arduino => "Arduino C++",
            ArkTS => "Ark TypeScript",
            Arturo => "Arturo",
            AsciiDoc => "AsciiDoc",
            Asn1 => "ASN.1",
            Asp => "ASP",
            AspNet => "ASP.NET",
            Assembly => "Assembly",
            AssemblyGAS => "GNU Style Assembly",
            Astro => "Astro",
            Ats => "ATS",
            Autoconf => "Autoconf",
            Autoit => "Autoit",
            AutoHotKey => "AutoHotKey",
            Automake => "Automake",
            AvaloniaXaml => "AXAML",
            AWK => "AWK",
            Ballerina => "Ballerina",
            Bash => "BASH",
            Batch => "Batch",
            Bazel => "Bazel",
            Bean => "Bean",
            Bicep => "Bicep",
            Bitbake => "Bitbake",
            Bqn => "BQN",
            BrightScript => "BrightScript",
            C => "C",
            Cabal => "Cabal",
            Cairo => "Cairo",
            Cangjie => "Cangjie",
            Cassius => "Cassius",
            Ceylon => "Ceylon",
            Chapel => "Chapel",
            CHeader => "C Header",
            Cil => "CIL (SELinux)",
            Circom => "Circom",
            Clojure => "Clojure",
            ClojureC => "ClojureC",
            ClojureScript => "ClojureScript",
            CMake => "CMake",
            Cobol => "COBOL",
            CodeQL => "CodeQL",
            CoffeeScript => "CoffeeScript",
            Cogent => "Cogent",
            ColdFusion => "ColdFusion",
            ColdFusionScript => "ColdFusion CFScript",
            Coq => "Coq",
            Cpp => "C++",
            CppHeader => "C++ Header",
            CppModule => "C++ Module",
            Crystal => "Crystal",
            CSharp => "C#",
            CShell => "C Shell",
            Css => "CSS",
            Cuda => "CUDA",
            Cue => "CUE",
            Cython => "Cython",
            D => "D",
            D2 => "D2",
            Daml => "DAML",
            Dart => "Dart",
            DeviceTree => "Device Tree",
            Dhall => "Dhall",
            Dockerfile => "Dockerfile",
            DotNetResource => ".NET Resource",
            DreamMaker => "Dream Maker",
            Dust => "Dust.js",
            Ebuild => "Ebuild",
            EdgeQL => "EdgeQL",
            ESDL => "EdgeDB Schema Definition",
            Edn => "Edn",
            Eighth => "8th",
            Elisp => "Emacs Lisp",
            Elixir => "Elixir",
            Elm => "Elm",
            Elvish => "Elvish",
            EmacsDevEnv => "Emacs Dev Env",
            Emojicode => "Emojicode",
            Erlang => "Erlang",
            Factor => "Factor",
            FEN => "FEN",
            Fennel => "Fennel",
            Fish => "Fish",
            FlatBuffers => "FlatBuffers Schema",
            ForgeConfig => "Forge Config",
            Forth => "Forth",
            FortranLegacy => "FORTRAN Legacy",
            FortranModern => "FORTRAN Modern",
            FreeMarker => "FreeMarker",
            FSharp => "F#",
            Fstar => "F*",
            Futhark => "Futhark",
            GDB => "GDB Script",
            GdScript => "GDScript",
            Gherkin => "Gherkin (Cucumber)",
            Gleam => "Gleam",
            GlimmerJs => "Glimmer JS",
            GlimmerTs => "Glimmer TS",
            Glsl => "GLSL",
            Gml => "Gml",
            Go => "Go",
            Gohtml => "Go HTML",
            Graphql => "GraphQL",
            Groovy => "Groovy",
            Gwion => "Gwion",
            Haml => "Haml",
            Hamlet => "Hamlet",
            Happy => "Happy",
            Handlebars => "Handlebars",
            Haskell => "Haskell",
            Haxe => "Haxe",
            Hcl => "HCL",
            Headache => "Headache",
            Hex => "HEX",
            Hex0 => "Hex0",
            Hex1 => "Hex1",
            Hex2 => "Hex2",
            HiCad => "HICAD",
            Hlsl => "HLSL",
            HolyC => "HolyC",
            Html => "HTML",
            Hy => "Hy",
            Idris => "Idris",
            Ini => "INI",
            IntelHex => "Intel HEX",
            Isabelle => "Isabelle",
            Jai => "JAI",
            Janet => "Janet",
            Java => "Java",
            JavaScript => "JavaScript",
            Jinja2 => "Jinja2",
            Jq => "jq",
            JSLT => "JSLT",
            Json => "JSON",
            Jsonnet => "Jsonnet",
            Jsx => "JSX",
            Julia => "Julia",
            Julius => "Julius",
            Jupyter => "Jupyter Notebooks",
            Just => "Just",
            K => "K",
            KakouneScript => "Kakoune script",
            Kaem => "Kaem",
            Koka => "Koka",
            Kotlin => "Kotlin",
            Ksh => "Korn shell",
            Lalrpop => "LALRPOP",
            KvLanguage => "KV Language",
            Lean => "Lean",
            Hledger => "hledger",
            Less => "LESS",
            Lex => "Lex",
            Liquid => "Liquid",
            LinguaFranca => "Lingua Franca",
            LinkerScript => "LD Script",
            Lisp => "Common Lisp",
            LiveScript => "LiveScript",
            LLVM => "LLVM",
            Logtalk => "Logtalk",
            LolCode => "LOLCODE",
            Lua => "Lua",
            Lucius => "Lucius",
            M1Assembly => "M1 Assembly",
            M4 => "M4",
            Madlang => "Madlang",
            Makefile => "Makefile",
            Markdown => "Markdown",
            Max => "Max",
            Mdx => "MDX",
            Menhir => "Menhir",
            Meson => "Meson",
            Metal => "Metal Shading Language",
            Mint => "Mint",
            Mlatu => "Mlatu",
            Modelica => "Modelica",
            ModuleDef => "Module-Definition",
            Mojo => "Mojo",
            MonkeyC => "Monkey C",
            MoonBit => "MoonBit",
            MoonScript => "MoonScript",
            MsBuild => "MSBuild",
            Mustache => "Mustache",
            Nextflow => "Nextflow",
            Nim => "Nim",
            Nix => "Nix",
            NotQuitePerl => "Not Quite Perl",
            NuGetConfig => "NuGet Config",
            Nushell => "Nushell",
            ObjectiveC => "Objective-C",
            ObjectiveCpp => "Objective-C++",
            OCaml => "OCaml",
            Odin => "Odin",
            OpenScad => "OpenSCAD",
            OpenPolicyAgent => "Open Policy Agent",
            OpenCL => "OpenCL",
            OpenQasm => "OpenQASM",
            OpenType => "OpenType Feature File",
            Org => "Org",
            Oz => "Oz",
            PacmanMakepkg => "Pacman's makepkg",
            Pan => "Pan",
            Pascal => "Pascal",
            Perl => "Perl",
            Pest => "Pest",
            Phix => "Phix",
            Php => "PHP",
            PlantUml => "PlantUML",
            Po => "PO File",
            Poke => "Poke",
            Polly => "Polly",
            Pony => "Pony",
            PostCss => "PostCSS",
            PowerShell => "PowerShell",
            PRACTICE => "Lauterbach PRACTICE Script",
            Processing => "Processing",
            Prolog => "Prolog",
            PSL => "PSL Assertion",
            Protobuf => "Protocol Buffers",
            Pug => "Pug",
            Puppet => "Puppet",
            PureScript => "PureScript",
            Pyret => "Pyret",
            Python => "Python",
            PRQL => "PRQL",
            Q => "Q",
            Qcl => "QCL",
            Qml => "QML",
            R => "R",
            Racket => "Racket",
            Rakefile => "Rakefile",
            Raku => "Raku",
            Razor => "Razor",
            Redscript => "Redscript",
            Renpy => "Ren'Py",
            ReScript => "ReScript",
            ReStructuredText => "ReStructuredText",
            Roc => "Roc",
            RON => "Rusty Object Notation",
            RPMSpecfile => "RPM Specfile",
            Ruby => "Ruby",
            RubyHtml => "Ruby HTML",
            Rust => "Rust",
            Sass => "Sass",
            Scala => "Scala",
            Scheme => "Scheme",
            Scons => "Scons",
            Sh => "Shell",
            ShaderLab => "ShaderLab",
            SIL => "SIL",
            Slang => "Slang",
            Sml => "Standard ML (SML)",
            Smalltalk => "Smalltalk",
            Snakemake => "Snakemake",
            Solidity => "Solidity",
            SpecmanE => "Specman e",
            Spice => "Spice Netlist",
            Sql => "SQL",
            Sqf => "SQF",
            SRecode => "SRecode Template",
            Stan => "Stan",
            Stata => "Stata",
            Stratego => "Stratego/XT",
            Stylus => "Stylus",
            Svelte => "Svelte",
            Svg => "SVG",
            Swift => "Swift",
            Swig => "SWIG",
            SystemVerilog => "SystemVerilog",
            Slint => "Slint",
            Tact => "Tact",
            Tcl => "TCL",
            Tera => "Tera",
            Templ => "Templ",
            Tex => "TeX",
            Text => "Plain Text",
            Thrift => "Thrift",
            Toml => "TOML",
            Tsx => "TSX",
            Ttcn => "TTCN-3",
            Twig => "Twig",
            TypeScript => "TypeScript",
            Typst => "Typst",
            Uiua => "Uiua",
            UMPL => "UMPL",
            Unison => "Unison",
            UnrealDeveloperMarkdown => "Unreal Markdown",
            UnrealPlugin => "Unreal Plugin",
            UnrealProject => "Unreal Project",
            UnrealScript => "Unreal Script",
            UnrealShader => "Unreal Shader",
            UnrealShaderHeader => "Unreal Shader Header",
            UrWeb => "Ur/Web",
            UrWebProject => "Ur/Web Project",
            Vala => "Vala",
            VB6 => "VB6/VBA",
            VBScript => "VBScript",
            Velocity => "Apache Velocity",
            Verilog => "Verilog",
            VerilogArgsFile => "Verilog Args File",
            Vhdl => "VHDL",
            Virgil => "Virgil",
            VisualBasic => "Visual Basic",
            VisualStudioProject => "Visual Studio Project",
            VisualStudioSolution => "Visual Studio Solution",
            VimScript => "Vim Script",
            Vue => "Vue",
            WebAssembly => "WebAssembly",
            WenYan => "The WenYan Programming Language",
            WGSL => "WebGPU Shader Language",
            Wolfram => "Wolfram",
            Xaml => "XAML",
            XcodeConfig => "Xcode Config",
            Xml => "XML",
            XSL => "XSL",
            Xtend => "Xtend",
            Yaml => "YAML",
            ZenCode => "ZenCode",
            Zig => "Zig",
            Zokrates => "ZoKrates",
            Zsh => "Zsh",
            GdShader => "GDShader",
            
        }
    }

    pub(crate) fn _is_blank(self) -> bool {
        match self {
            Abap => false,
            ABNF => false,
            ActionScript => false,
            Ada => false,
            Agda => false,
            Alex => false,
            Alloy => false,
            Apl => false,
            Arduino => false,
            ArkTS => false,
            Arturo => false,
            AsciiDoc => false,
            Asn1 => false,
            Asp => false,
            AspNet => false,
            Assembly => false,
            AssemblyGAS => false,
            Astro => false,
            Ats => false,
            Autoconf => false,
            Autoit => false,
            AutoHotKey => false,
            Automake => false,
            AvaloniaXaml => false,
            AWK => false,
            Ballerina => false,
            Bash => false,
            Batch => false,
            Bazel => false,
            Bean => false,
            Bicep => false,
            Bitbake => false,
            Bqn => false,
            BrightScript => false,
            C => false,
            Cabal => false,
            Cairo => false,
            Cangjie => false,
            Cassius => false,
            Ceylon => false,
            Chapel => false,
            CHeader => false,
            Cil => false,
            Circom => false,
            Clojure => false,
            ClojureC => false,
            ClojureScript => false,
            CMake => false,
            Cobol => false,
            CodeQL => false,
            CoffeeScript => false,
            Cogent => false,
            ColdFusion => false,
            ColdFusionScript => false,
            Coq => false,
            Cpp => false,
            CppHeader => false,
            CppModule => false,
            Crystal => false,
            CSharp => false,
            CShell => false,
            Css => false,
            Cuda => false,
            Cue => false,
            Cython => false,
            D => false,
            D2 => false,
            Daml => false,
            Dart => false,
            DeviceTree => false,
            Dhall => false,
            Dockerfile => false,
            DotNetResource => false,
            DreamMaker => false,
            Dust => false,
            Ebuild => false,
            EdgeQL => false,
            ESDL => false,
            Edn => false,
            Eighth => false,
            Elisp => false,
            Elixir => false,
            Elm => false,
            Elvish => false,
            EmacsDevEnv => false,
            Emojicode => false,
            Erlang => false,
            Factor => false,
            FEN => true,
            Fennel => false,
            Fish => false,
            FlatBuffers => false,
            ForgeConfig => false,
            Forth => false,
            FortranLegacy => false,
            FortranModern => false,
            FreeMarker => false,
            FSharp => false,
            Fstar => false,
            Futhark => false,
            GDB => false,
            GdScript => false,
            Gherkin => false,
            Gleam => false,
            GlimmerJs => false,
            GlimmerTs => false,
            Glsl => false,
            Gml => false,
            Go => false,
            Gohtml => false,
            Graphql => false,
            Groovy => false,
            Gwion => false,
            Haml => false,
            Hamlet => false,
            Happy => false,
            Handlebars => false,
            Haskell => false,
            Haxe => false,
            Hcl => false,
            Headache => false,
            Hex => true,
            Hex0 => false,
            Hex1 => false,
            Hex2 => false,
            HiCad => false,
            Hlsl => false,
            HolyC => false,
            Html => false,
            Hy => false,
            Idris => false,
            Ini => false,
            IntelHex => true,
            Isabelle => false,
            Jai => false,
            Janet => false,
            Java => false,
            JavaScript => false,
            Jinja2 => true,
            Jq => false,
            JSLT => false,
            Json => true,
            Jsonnet => false,
            Jsx => false,
            Julia => false,
            Julius => false,
            Jupyter => false,
            Just => false,
            K => false,
            KakouneScript => false,
            Kaem => false,
            Koka => false,
            Kotlin => false,
            Ksh => false,
            Lalrpop => false,
            KvLanguage => false,
            Lean => false,
            Hledger => false,
            Less => false,
            Lex => false,
            Liquid => false,
            LinguaFranca => false,
            LinkerScript => false,
            Lisp => false,
            LiveScript => false,
            LLVM => false,
            Logtalk => false,
            LolCode => false,
            Lua => false,
            Lucius => false,
            M1Assembly => false,
            M4 => false,
            Madlang => false,
            Makefile => false,
            Markdown => false,
            Max => false,
            Mdx => false,
            Menhir => false,
            Meson => false,
            Metal => false,
            Mint => true,
            Mlatu => false,
            Modelica => false,
            ModuleDef => false,
            Mojo => false,
            MonkeyC => false,
            MoonBit => false,
            MoonScript => false,
            MsBuild => false,
            Mustache => false,
            Nextflow => false,
            Nim => false,
            Nix => false,
            NotQuitePerl => false,
            NuGetConfig => false,
            Nushell => false,
            ObjectiveC => false,
            ObjectiveCpp => false,
            OCaml => false,
            Odin => false,
            OpenScad => false,
            OpenPolicyAgent => false,
            OpenCL => false,
            OpenQasm => false,
            OpenType => false,
            Org => false,
            Oz => false,
            PacmanMakepkg => false,
            Pan => false,
            Pascal => false,
            Perl => false,
            Pest => false,
            Phix => false,
            Php => false,
            PlantUml => false,
            Po => false,
            Poke => false,
            Polly => false,
            Pony => false,
            PostCss => false,
            PowerShell => false,
            PRACTICE => false,
            Processing => false,
            Prolog => false,
            PSL => false,
            Protobuf => false,
            Pug => false,
            Puppet => false,
            PureScript => false,
            Pyret => false,
            Python => false,
            PRQL => false,
            Q => false,
            Qcl => false,
            Qml => false,
            R => false,
            Racket => false,
            Rakefile => false,
            Raku => false,
            Razor => false,
            Redscript => false,
            Renpy => false,
            ReScript => false,
            ReStructuredText => true,
            Roc => false,
            RON => false,
            RPMSpecfile => false,
            Ruby => false,
            RubyHtml => false,
            Rust => false,
            Sass => false,
            Scala => false,
            Scheme => false,
            Scons => false,
            Sh => false,
            ShaderLab => false,
            SIL => false,
            Slang => false,
            Sml => false,
            Smalltalk => false,
            Snakemake => false,
            Solidity => false,
            SpecmanE => false,
            Spice => false,
            Sql => false,
            Sqf => false,
            SRecode => false,
            Stan => false,
            Stata => false,
            Stratego => false,
            Stylus => false,
            Svelte => false,
            Svg => false,
            Swift => false,
            Swig => false,
            SystemVerilog => false,
            Slint => false,
            Tact => false,
            Tcl => false,
            Tera => false,
            Templ => false,
            Tex => false,
            Text => false,
            Thrift => false,
            Toml => false,
            Tsx => false,
            Ttcn => false,
            Twig => false,
            TypeScript => false,
            Typst => false,
            Uiua => false,
            UMPL => false,
            Unison => false,
            UnrealDeveloperMarkdown => false,
            UnrealPlugin => true,
            UnrealProject => true,
            UnrealScript => false,
            UnrealShader => false,
            UnrealShaderHeader => false,
            UrWeb => false,
            UrWebProject => false,
            Vala => false,
            VB6 => false,
            VBScript => false,
            Velocity => false,
            Verilog => false,
            VerilogArgsFile => false,
            Vhdl => false,
            Virgil => false,
            VisualBasic => false,
            VisualStudioProject => false,
            VisualStudioSolution => true,
            VimScript => false,
            Vue => false,
            WebAssembly => false,
            WenYan => false,
            WGSL => false,
            Wolfram => false,
            Xaml => false,
            XcodeConfig => false,
            Xml => false,
            XSL => false,
            Xtend => false,
            Yaml => false,
            ZenCode => false,
            Zig => false,
            Zokrates => false,
            Zsh => false,
            GdShader => false,
            
        }
    }

    pub(crate) fn is_fortran(self) -> bool {
        self == LanguageType::FortranModern ||
        self == LanguageType::FortranLegacy
    }

    /// Returns whether the language is "literate", meaning that it considered
    /// to primarily be documentation and is counted primarily as comments
    /// rather than procedural code.
    pub fn is_literate(self) -> bool {
        match self {
            Abap => false,
            ABNF => false,
            ActionScript => false,
            Ada => false,
            Agda => false,
            Alex => false,
            Alloy => false,
            Apl => false,
            Arduino => false,
            ArkTS => false,
            Arturo => false,
            AsciiDoc => false,
            Asn1 => false,
            Asp => false,
            AspNet => false,
            Assembly => false,
            AssemblyGAS => false,
            Astro => false,
            Ats => false,
            Autoconf => false,
            Autoit => false,
            AutoHotKey => false,
            Automake => false,
            AvaloniaXaml => false,
            AWK => false,
            Ballerina => false,
            Bash => false,
            Batch => false,
            Bazel => false,
            Bean => false,
            Bicep => false,
            Bitbake => false,
            Bqn => false,
            BrightScript => false,
            C => false,
            Cabal => false,
            Cairo => false,
            Cangjie => false,
            Cassius => false,
            Ceylon => false,
            Chapel => false,
            CHeader => false,
            Cil => false,
            Circom => false,
            Clojure => false,
            ClojureC => false,
            ClojureScript => false,
            CMake => false,
            Cobol => false,
            CodeQL => false,
            CoffeeScript => false,
            Cogent => false,
            ColdFusion => false,
            ColdFusionScript => false,
            Coq => false,
            Cpp => false,
            CppHeader => false,
            CppModule => false,
            Crystal => false,
            CSharp => false,
            CShell => false,
            Css => false,
            Cuda => false,
            Cue => false,
            Cython => false,
            D => false,
            D2 => false,
            Daml => false,
            Dart => false,
            DeviceTree => false,
            Dhall => false,
            Dockerfile => false,
            DotNetResource => false,
            DreamMaker => false,
            Dust => false,
            Ebuild => false,
            EdgeQL => false,
            ESDL => false,
            Edn => false,
            Eighth => false,
            Elisp => false,
            Elixir => false,
            Elm => false,
            Elvish => false,
            EmacsDevEnv => false,
            Emojicode => false,
            Erlang => false,
            Factor => false,
            FEN => false,
            Fennel => false,
            Fish => false,
            FlatBuffers => false,
            ForgeConfig => false,
            Forth => false,
            FortranLegacy => false,
            FortranModern => false,
            FreeMarker => false,
            FSharp => false,
            Fstar => false,
            Futhark => false,
            GDB => false,
            GdScript => false,
            Gherkin => false,
            Gleam => false,
            GlimmerJs => false,
            GlimmerTs => false,
            Glsl => false,
            Gml => false,
            Go => false,
            Gohtml => false,
            Graphql => false,
            Groovy => false,
            Gwion => false,
            Haml => false,
            Hamlet => false,
            Happy => false,
            Handlebars => false,
            Haskell => false,
            Haxe => false,
            Hcl => false,
            Headache => false,
            Hex => false,
            Hex0 => false,
            Hex1 => false,
            Hex2 => false,
            HiCad => false,
            Hlsl => false,
            HolyC => false,
            Html => false,
            Hy => false,
            Idris => false,
            Ini => false,
            IntelHex => false,
            Isabelle => false,
            Jai => false,
            Janet => false,
            Java => false,
            JavaScript => false,
            Jinja2 => false,
            Jq => false,
            JSLT => false,
            Json => false,
            Jsonnet => false,
            Jsx => false,
            Julia => false,
            Julius => false,
            Jupyter => false,
            Just => false,
            K => false,
            KakouneScript => false,
            Kaem => false,
            Koka => false,
            Kotlin => false,
            Ksh => false,
            Lalrpop => false,
            KvLanguage => false,
            Lean => false,
            Hledger => false,
            Less => false,
            Lex => false,
            Liquid => false,
            LinguaFranca => false,
            LinkerScript => false,
            Lisp => false,
            LiveScript => false,
            LLVM => false,
            Logtalk => false,
            LolCode => false,
            Lua => false,
            Lucius => false,
            M1Assembly => false,
            M4 => false,
            Madlang => false,
            Makefile => false,
            Markdown => true,
            Max => false,
            Mdx => true,
            Menhir => false,
            Meson => false,
            Metal => false,
            Mint => false,
            Mlatu => false,
            Modelica => false,
            ModuleDef => false,
            Mojo => false,
            MonkeyC => false,
            MoonBit => false,
            MoonScript => false,
            MsBuild => false,
            Mustache => false,
            Nextflow => false,
            Nim => false,
            Nix => false,
            NotQuitePerl => false,
            NuGetConfig => false,
            Nushell => false,
            ObjectiveC => false,
            ObjectiveCpp => false,
            OCaml => false,
            Odin => false,
            OpenScad => false,
            OpenPolicyAgent => false,
            OpenCL => false,
            OpenQasm => false,
            OpenType => false,
            Org => false,
            Oz => false,
            PacmanMakepkg => false,
            Pan => false,
            Pascal => false,
            Perl => false,
            Pest => false,
            Phix => false,
            Php => false,
            PlantUml => false,
            Po => false,
            Poke => false,
            Polly => false,
            Pony => false,
            PostCss => false,
            PowerShell => false,
            PRACTICE => false,
            Processing => false,
            Prolog => false,
            PSL => false,
            Protobuf => false,
            Pug => false,
            Puppet => false,
            PureScript => false,
            Pyret => false,
            Python => false,
            PRQL => false,
            Q => false,
            Qcl => false,
            Qml => false,
            R => false,
            Racket => false,
            Rakefile => false,
            Raku => false,
            Razor => false,
            Redscript => false,
            Renpy => false,
            ReScript => false,
            ReStructuredText => false,
            Roc => false,
            RON => false,
            RPMSpecfile => false,
            Ruby => false,
            RubyHtml => false,
            Rust => false,
            Sass => false,
            Scala => false,
            Scheme => false,
            Scons => false,
            Sh => false,
            ShaderLab => false,
            SIL => false,
            Slang => false,
            Sml => false,
            Smalltalk => false,
            Snakemake => false,
            Solidity => false,
            SpecmanE => false,
            Spice => false,
            Sql => false,
            Sqf => false,
            SRecode => false,
            Stan => false,
            Stata => false,
            Stratego => false,
            Stylus => false,
            Svelte => false,
            Svg => false,
            Swift => false,
            Swig => false,
            SystemVerilog => false,
            Slint => false,
            Tact => false,
            Tcl => false,
            Tera => false,
            Templ => false,
            Tex => false,
            Text => true,
            Thrift => false,
            Toml => false,
            Tsx => false,
            Ttcn => false,
            Twig => false,
            TypeScript => false,
            Typst => false,
            Uiua => false,
            UMPL => false,
            Unison => false,
            UnrealDeveloperMarkdown => false,
            UnrealPlugin => false,
            UnrealProject => false,
            UnrealScript => false,
            UnrealShader => false,
            UnrealShaderHeader => false,
            UrWeb => false,
            UrWebProject => false,
            Vala => false,
            VB6 => false,
            VBScript => false,
            Velocity => false,
            Verilog => false,
            VerilogArgsFile => false,
            Vhdl => false,
            Virgil => false,
            VisualBasic => false,
            VisualStudioProject => false,
            VisualStudioSolution => false,
            VimScript => false,
            Vue => false,
            WebAssembly => false,
            WenYan => false,
            WGSL => false,
            Wolfram => false,
            Xaml => false,
            XcodeConfig => false,
            Xml => false,
            XSL => false,
            Xtend => false,
            Yaml => false,
            ZenCode => false,
            Zig => false,
            Zokrates => false,
            Zsh => false,
            GdShader => false,
            
        }
    }

    /// Provides every variant in a Vec
    pub fn list() -> &'static [(Self, &'static [&'static str])] {
        &[(Abap,
             &["abap", ],
            ),
        (ABNF,
             &["abnf", ],
            ),
        (ActionScript,
             &["as", ],
            ),
        (Ada,
             &["ada", "adb", "ads", "pad", ],
            ),
        (Agda,
             &["agda", ],
            ),
        (Alex,
             &["x", ],
            ),
        (Alloy,
             &["als", ],
            ),
        (Apl,
             &["apl", "aplf", "apls", ],
            ),
        (Arduino,
             &["ino", ],
            ),
        (ArkTS,
             &["ets", ],
            ),
        (Arturo,
             &["art", ],
            ),
        (AsciiDoc,
             &["adoc", "asciidoc", ],
            ),
        (Asn1,
             &["asn1", ],
            ),
        (Asp,
             &["asa", "asp", ],
            ),
        (AspNet,
             &["asax", "ascx", "asmx", "aspx", "master", "sitemap", "webinfo", ],
            ),
        (Assembly,
             &["asm", ],
            ),
        (AssemblyGAS,
             &["s", ],
            ),
        (Astro,
             &["astro", ],
            ),
        (Ats,
             &["dats", "hats", "sats", "atxt", ],
            ),
        (Autoconf,
             &["in", ],
            ),
        (Autoit,
             &["au3", ],
            ),
        (AutoHotKey,
             &["ahk", ],
            ),
        (Automake,
             &["am", ],
            ),
        (AvaloniaXaml,
             &["axaml", ],
            ),
        (AWK,
             &["awk", ],
            ),
        (Ballerina,
             &["bal", ],
            ),
        (Bash,
             &["bash", ],
            ),
        (Batch,
             &["bat", "btm", "cmd", ],
            ),
        (Bazel,
             &["bzl", "bazel", "bzlmod", ],
            ),
        (Bean,
             &["bean", "beancount", ],
            ),
        (Bicep,
             &["bicep", "bicepparam", ],
            ),
        (Bitbake,
             &["bb", "bbclass", "bbappend", "inc", ],
            ),
        (Bqn,
             &["bqn", ],
            ),
        (BrightScript,
             &["brs", ],
            ),
        (C,
             &["c", "ec", "pgc", ],
            ),
        (Cabal,
             &["cabal", ],
            ),
        (Cairo,
             &["cairo", ],
            ),
        (Cangjie,
             &["cj", ],
            ),
        (Cassius,
             &["cassius", ],
            ),
        (Ceylon,
             &["ceylon", ],
            ),
        (Chapel,
             &["chpl", ],
            ),
        (CHeader,
             &["h", ],
            ),
        (Cil,
             &["cil", ],
            ),
        (Circom,
             &["circom", ],
            ),
        (Clojure,
             &["clj", ],
            ),
        (ClojureC,
             &["cljc", ],
            ),
        (ClojureScript,
             &["cljs", ],
            ),
        (CMake,
             &["cmake", ],
            ),
        (Cobol,
             &["cob", "cbl", "ccp", "cobol", "cpy", ],
            ),
        (CodeQL,
             &["ql", "qll", ],
            ),
        (CoffeeScript,
             &["coffee", "cjsx", ],
            ),
        (Cogent,
             &["cogent", ],
            ),
        (ColdFusion,
             &["cfm", ],
            ),
        (ColdFusionScript,
             &["cfc", ],
            ),
        (Coq,
             &["v", ],
            ),
        (Cpp,
             &["cc", "cpp", "cxx", "c++", "pcc", "tpp", ],
            ),
        (CppHeader,
             &["hh", "hpp", "hxx", "inl", "ipp", ],
            ),
        (CppModule,
             &["cppm", "ixx", "ccm", "mpp", "mxx", "cxxm", "hppm", "hxxm", ],
            ),
        (Crystal,
             &["cr", ],
            ),
        (CSharp,
             &["cs", "csx", ],
            ),
        (CShell,
             &["csh", ],
            ),
        (Css,
             &["css", ],
            ),
        (Cuda,
             &["cu", ],
            ),
        (Cue,
             &["cue", ],
            ),
        (Cython,
             &["pyx", "pxd", "pxi", ],
            ),
        (D,
             &["d", ],
            ),
        (D2,
             &["d2", ],
            ),
        (Daml,
             &["daml", ],
            ),
        (Dart,
             &["dart", ],
            ),
        (DeviceTree,
             &["dts", "dtsi", ],
            ),
        (Dhall,
             &["dhall", ],
            ),
        (Dockerfile,
             &["dockerfile", "dockerignore", ],
            ),
        (DotNetResource,
             &["resx", ],
            ),
        (DreamMaker,
             &["dm", "dme", ],
            ),
        (Dust,
             &["dust", ],
            ),
        (Ebuild,
             &["ebuild", "eclass", ],
            ),
        (EdgeQL,
             &["edgeql", ],
            ),
        (ESDL,
             &["esdl", ],
            ),
        (Edn,
             &["edn", ],
            ),
        (Eighth,
             &["8th", ],
            ),
        (Elisp,
             &["el", ],
            ),
        (Elixir,
             &["ex", "exs", ],
            ),
        (Elm,
             &["elm", ],
            ),
        (Elvish,
             &["elv", ],
            ),
        (EmacsDevEnv,
             &["ede", ],
            ),
        (Emojicode,
             &["emojic", "🍇", ],
            ),
        (Erlang,
             &["erl", "hrl", ],
            ),
        (Factor,
             &["factor", ],
            ),
        (FEN,
             &["fen", ],
            ),
        (Fennel,
             &["fnl", "fnlm", ],
            ),
        (Fish,
             &["fish", ],
            ),
        (FlatBuffers,
             &["fbs", ],
            ),
        (ForgeConfig,
             &["cfg", ],
            ),
        (Forth,
             &["4th", "forth", "fr", "frt", "fth", "f83", "fb", "fpm", "e4", "rx", "ft", ],
            ),
        (FortranLegacy,
             &["f", "for", "ftn", "f77", "pfo", ],
            ),
        (FortranModern,
             &["f03", "f08", "f90", "f95", "fpp", ],
            ),
        (FreeMarker,
             &["ftl", "ftlh", "ftlx", ],
            ),
        (FSharp,
             &["fs", "fsi", "fsx", "fsscript", ],
            ),
        (Fstar,
             &["fst", "fsti", ],
            ),
        (Futhark,
             &["fut", ],
            ),
        (GDB,
             &["gdb", ],
            ),
        (GdScript,
             &["gd", ],
            ),
        (Gherkin,
             &["feature", ],
            ),
        (Gleam,
             &["gleam", ],
            ),
        (GlimmerJs,
             &["gjs", ],
            ),
        (GlimmerTs,
             &["gts", ],
            ),
        (Glsl,
             &["vert", "tesc", "tese", "geom", "frag", "comp", "mesh", "task", "rgen", "rint", "rahit", "rchit", "rmiss", "rcall", "glsl", ],
            ),
        (Gml,
             &["gml", ],
            ),
        (Go,
             &["go", ],
            ),
        (Gohtml,
             &["gohtml", ],
            ),
        (Graphql,
             &["gql", "graphql", ],
            ),
        (Groovy,
             &["groovy", "grt", "gtpl", "gvy", ],
            ),
        (Gwion,
             &["gw", ],
            ),
        (Haml,
             &["haml", ],
            ),
        (Hamlet,
             &["hamlet", ],
            ),
        (Happy,
             &["y", "ly", ],
            ),
        (Handlebars,
             &["hbs", "handlebars", ],
            ),
        (Haskell,
             &["hs", ],
            ),
        (Haxe,
             &["hx", ],
            ),
        (Hcl,
             &["hcl", "tf", "tfvars", ],
            ),
        (Headache,
             &["ha", ],
            ),
        (Hex,
             &["hex", ],
            ),
        (Hex0,
             &["hex0", ],
            ),
        (Hex1,
             &["hex1", ],
            ),
        (Hex2,
             &["hex2", ],
            ),
        (HiCad,
             &["MAC", "mac", ],
            ),
        (Hlsl,
             &["hlsl", "fx", "fxsub", ],
            ),
        (HolyC,
             &["HC", "hc", "ZC", "zc", ],
            ),
        (Html,
             &["html", "htm", ],
            ),
        (Hy,
             &["hy", ],
            ),
        (Idris,
             &["idr", "lidr", ],
            ),
        (Ini,
             &["ini", ],
            ),
        (IntelHex,
             &["ihex", ],
            ),
        (Isabelle,
             &["thy", ],
            ),
        (Jai,
             &["jai", ],
            ),
        (Janet,
             &["janet", ],
            ),
        (Java,
             &["java", ],
            ),
        (JavaScript,
             &["cjs", "js", "mjs", ],
            ),
        (Jinja2,
             &["j2", "jinja", ],
            ),
        (Jq,
             &["jq", ],
            ),
        (JSLT,
             &["jslt", ],
            ),
        (Json,
             &["json", ],
            ),
        (Jsonnet,
             &["jsonnet", "libsonnet", ],
            ),
        (Jsx,
             &["jsx", ],
            ),
        (Julia,
             &["jl", ],
            ),
        (Julius,
             &["julius", ],
            ),
        (Jupyter,
             &["ipynb", ],
            ),
        (Just,
             &["just", ],
            ),
        (K,
             &["k", ],
            ),
        (KakouneScript,
             &["kak", ],
            ),
        (Kaem,
             &["kaem", ],
            ),
        (Koka,
             &["kk", ],
            ),
        (Kotlin,
             &["kt", "kts", ],
            ),
        (Ksh,
             &["ksh", ],
            ),
        (Lalrpop,
             &["lalrpop", ],
            ),
        (KvLanguage,
             &["kv", ],
            ),
        (Lean,
             &["lean", "hlean", ],
            ),
        (Hledger,
             &["hledger", ],
            ),
        (Less,
             &["less", ],
            ),
        (Lex,
             &["l", "lex", ],
            ),
        (Liquid,
             &["liquid", ],
            ),
        (LinguaFranca,
             &["lf", ],
            ),
        (LinkerScript,
             &["ld", "lds", ],
            ),
        (Lisp,
             &["lisp", "lsp", "asd", ],
            ),
        (LiveScript,
             &["ls", ],
            ),
        (LLVM,
             &["ll", ],
            ),
        (Logtalk,
             &["lgt", "logtalk", ],
            ),
        (LolCode,
             &["lol", ],
            ),
        (Lua,
             &["lua", "luau", ],
            ),
        (Lucius,
             &["lucius", ],
            ),
        (M1Assembly,
             &["m1", ],
            ),
        (M4,
             &["m4", ],
            ),
        (Madlang,
             &["mad", ],
            ),
        (Makefile,
             &["makefile", "mak", "mk", ],
            ),
        (Markdown,
             &["md", "markdown", ],
            ),
        (Max,
             &["maxpat", ],
            ),
        (Mdx,
             &["mdx", ],
            ),
        (Menhir,
             &["mll", "mly", "vy", ],
            ),
        (Meson,
             &[],
            ),
        (Metal,
             &["metal", ],
            ),
        (Mint,
             &["mint", ],
            ),
        (Mlatu,
             &["mlt", ],
            ),
        (Modelica,
             &["mo", "mos", ],
            ),
        (ModuleDef,
             &["def", ],
            ),
        (Mojo,
             &["mojo", "🔥", ],
            ),
        (MonkeyC,
             &["mc", ],
            ),
        (MoonBit,
             &["mbt", "mbti", ],
            ),
        (MoonScript,
             &["moon", ],
            ),
        (MsBuild,
             &["csproj", "vbproj", "fsproj", "props", "targets", ],
            ),
        (Mustache,
             &["mustache", ],
            ),
        (Nextflow,
             &["nextflow", "nf", ],
            ),
        (Nim,
             &["nim", ],
            ),
        (Nix,
             &["nix", ],
            ),
        (NotQuitePerl,
             &["nqp", ],
            ),
        (NuGetConfig,
             &[],
            ),
        (Nushell,
             &["nu", ],
            ),
        (ObjectiveC,
             &["m", ],
            ),
        (ObjectiveCpp,
             &["mm", ],
            ),
        (OCaml,
             &["ml", "mli", "re", "rei", ],
            ),
        (Odin,
             &["odin", ],
            ),
        (OpenScad,
             &["scad", ],
            ),
        (OpenPolicyAgent,
             &["rego", ],
            ),
        (OpenCL,
             &["cl", "ocl", ],
            ),
        (OpenQasm,
             &["qasm", ],
            ),
        (OpenType,
             &["fea", ],
            ),
        (Org,
             &["org", ],
            ),
        (Oz,
             &["oz", ],
            ),
        (PacmanMakepkg,
             &[],
            ),
        (Pan,
             &["pan", "tpl", ],
            ),
        (Pascal,
             &["pas", ],
            ),
        (Perl,
             &["pl", "pm", ],
            ),
        (Pest,
             &["pest", ],
            ),
        (Phix,
             &["e", "exw", ],
            ),
        (Php,
             &["php", ],
            ),
        (PlantUml,
             &["puml", ],
            ),
        (Po,
             &["po", "pot", ],
            ),
        (Poke,
             &["pk", ],
            ),
        (Polly,
             &["polly", ],
            ),
        (Pony,
             &["pony", ],
            ),
        (PostCss,
             &["pcss", "sss", ],
            ),
        (PowerShell,
             &["ps1", "psm1", "psd1", "ps1xml", "cdxml", "pssc", "psc1", ],
            ),
        (PRACTICE,
             &["cmm", ],
            ),
        (Processing,
             &["pde", ],
            ),
        (Prolog,
             &["p", "pro", ],
            ),
        (PSL,
             &["psl", ],
            ),
        (Protobuf,
             &["proto", ],
            ),
        (Pug,
             &["pug", ],
            ),
        (Puppet,
             &["pp", ],
            ),
        (PureScript,
             &["purs", ],
            ),
        (Pyret,
             &["arr", ],
            ),
        (Python,
             &["py", "pyw", "pyi", ],
            ),
        (PRQL,
             &["prql", ],
            ),
        (Q,
             &["q", ],
            ),
        (Qcl,
             &["qcl", ],
            ),
        (Qml,
             &["qml", ],
            ),
        (R,
             &["r", ],
            ),
        (Racket,
             &["rkt", "scrbl", ],
            ),
        (Rakefile,
             &["rake", ],
            ),
        (Raku,
             &["raku", "rakumod", "rakutest", "pm6", "pl6", "p6", ],
            ),
        (Razor,
             &["cshtml", "razor", ],
            ),
        (Redscript,
             &["reds", ],
            ),
        (Renpy,
             &["rpy", ],
            ),
        (ReScript,
             &["res", "resi", ],
            ),
        (ReStructuredText,
             &["rst", ],
            ),
        (Roc,
             &["roc", ],
            ),
        (RON,
             &["ron", ],
            ),
        (RPMSpecfile,
             &["spec", ],
            ),
        (Ruby,
             &["rb", ],
            ),
        (RubyHtml,
             &["rhtml", "erb", ],
            ),
        (Rust,
             &["rs", ],
            ),
        (Sass,
             &["sass", "scss", ],
            ),
        (Scala,
             &["sc", "scala", ],
            ),
        (Scheme,
             &["scm", "ss", ],
            ),
        (Scons,
             &[],
            ),
        (Sh,
             &["sh", ],
            ),
        (ShaderLab,
             &["shader", "cginc", ],
            ),
        (SIL,
             &["sil", ],
            ),
        (Slang,
             &["slang", ],
            ),
        (Sml,
             &["sml", ],
            ),
        (Smalltalk,
             &["cs.st", "pck.st", ],
            ),
        (Snakemake,
             &["smk", "rules", ],
            ),
        (Solidity,
             &["sol", ],
            ),
        (SpecmanE,
             &["e", ],
            ),
        (Spice,
             &["ckt", ],
            ),
        (Sql,
             &["sql", ],
            ),
        (Sqf,
             &["sqf", ],
            ),
        (SRecode,
             &["srt", ],
            ),
        (Stan,
             &["stan", ],
            ),
        (Stata,
             &["do", ],
            ),
        (Stratego,
             &["str", ],
            ),
        (Stylus,
             &["styl", ],
            ),
        (Svelte,
             &["svelte", ],
            ),
        (Svg,
             &["svg", ],
            ),
        (Swift,
             &["swift", ],
            ),
        (Swig,
             &["swg", "i", ],
            ),
        (SystemVerilog,
             &["sv", "svh", ],
            ),
        (Slint,
             &["slint", ],
            ),
        (Tact,
             &["tact", ],
            ),
        (Tcl,
             &["tcl", ],
            ),
        (Tera,
             &["tera", ],
            ),
        (Templ,
             &["templ", "tmpl", ],
            ),
        (Tex,
             &["tex", "sty", ],
            ),
        (Text,
             &["text", "txt", ],
            ),
        (Thrift,
             &["thrift", ],
            ),
        (Toml,
             &["toml", ],
            ),
        (Tsx,
             &["tsx", ],
            ),
        (Ttcn,
             &["ttcn", "ttcn3", "ttcnpp", ],
            ),
        (Twig,
             &["twig", ],
            ),
        (TypeScript,
             &["ts", "mts", "cts", ],
            ),
        (Typst,
             &["typ", ],
            ),
        (Uiua,
             &["ua", ],
            ),
        (UMPL,
             &["umpl", ],
            ),
        (Unison,
             &["u", ],
            ),
        (UnrealDeveloperMarkdown,
             &["udn", ],
            ),
        (UnrealPlugin,
             &["uplugin", ],
            ),
        (UnrealProject,
             &["uproject", ],
            ),
        (UnrealScript,
             &["uc", "uci", "upkg", ],
            ),
        (UnrealShader,
             &["usf", ],
            ),
        (UnrealShaderHeader,
             &["ush", ],
            ),
        (UrWeb,
             &["ur", "urs", ],
            ),
        (UrWebProject,
             &["urp", ],
            ),
        (Vala,
             &["vala", ],
            ),
        (VB6,
             &["frm", "bas", "cls", "ctl", "dsr", ],
            ),
        (VBScript,
             &["vbs", ],
            ),
        (Velocity,
             &["vm", ],
            ),
        (Verilog,
             &["vg", "vh", ],
            ),
        (VerilogArgsFile,
             &["irunargs", "xrunargs", ],
            ),
        (Vhdl,
             &["vhd", "vhdl", ],
            ),
        (Virgil,
             &["v3", ],
            ),
        (VisualBasic,
             &["vb", ],
            ),
        (VisualStudioProject,
             &["vcproj", "vcxproj", ],
            ),
        (VisualStudioSolution,
             &["sln", ],
            ),
        (VimScript,
             &["vim", ],
            ),
        (Vue,
             &["vue", ],
            ),
        (WebAssembly,
             &["wat", "wast", ],
            ),
        (WenYan,
             &["wy", ],
            ),
        (WGSL,
             &["wgsl", ],
            ),
        (Wolfram,
             &["nb", "wl", ],
            ),
        (Xaml,
             &["xaml", ],
            ),
        (XcodeConfig,
             &["xcconfig", ],
            ),
        (Xml,
             &["xml", ],
            ),
        (XSL,
             &["xsl", "xslt", ],
            ),
        (Xtend,
             &["xtend", ],
            ),
        (Yaml,
             &["yaml", "yml", ],
            ),
        (ZenCode,
             &["zs", ],
            ),
        (Zig,
             &["zig", ],
            ),
        (Zokrates,
             &["zok", ],
            ),
        (Zsh,
             &["zsh", ],
            ),
        (GdShader,
             &["gdshader", ],
            ),
        ]
    }

    /// Returns the single line comments of a language.
    /// ```
    /// use tokei::LanguageType;
    /// let lang = LanguageType::Rust;
    /// assert_eq!(lang.line_comments(), &["//"]);
    /// ```
    pub fn line_comments(self) -> &'static [&'static str] {
        match self {
            Abap => &["*","\"",],
            ABNF => &[";",],
            ActionScript => &["//",],
            Ada => &["--",],
            Agda => &["--",],
            Alex => &[],
            Alloy => &["--","//",],
            Apl => &["⍝",],
            Arduino => &["//",],
            ArkTS => &["//",],
            Arturo => &[";",],
            AsciiDoc => &["//",],
            Asn1 => &["--",],
            Asp => &["'","REM",],
            AspNet => &[],
            Assembly => &[";",],
            AssemblyGAS => &["//",],
            Astro => &["//",],
            Ats => &["//",],
            Autoconf => &["#","dnl",],
            Autoit => &[";",],
            AutoHotKey => &[";",],
            Automake => &["#",],
            AvaloniaXaml => &[],
            AWK => &["#",],
            Ballerina => &["//","#",],
            Bash => &["#",],
            Batch => &["REM","::",],
            Bazel => &["#",],
            Bean => &[";",],
            Bicep => &["//",],
            Bitbake => &["#",],
            Bqn => &["#",],
            BrightScript => &["'","REM",],
            C => &["//",],
            Cabal => &["--",],
            Cairo => &["//",],
            Cangjie => &["//",],
            Cassius => &["//",],
            Ceylon => &["//",],
            Chapel => &["//",],
            CHeader => &["//",],
            Cil => &[";",],
            Circom => &["//",],
            Clojure => &[";",],
            ClojureC => &[";",],
            ClojureScript => &[";",],
            CMake => &["#",],
            Cobol => &["*",],
            CodeQL => &["//",],
            CoffeeScript => &["#",],
            Cogent => &["--",],
            ColdFusion => &[],
            ColdFusionScript => &["//",],
            Coq => &[],
            Cpp => &["//",],
            CppHeader => &["//",],
            CppModule => &["//",],
            Crystal => &["#",],
            CSharp => &["//",],
            CShell => &["#",],
            Css => &["//",],
            Cuda => &["//",],
            Cue => &["//",],
            Cython => &["#",],
            D => &["//",],
            D2 => &["#",],
            Daml => &["-- ",],
            Dart => &["//",],
            DeviceTree => &["//",],
            Dhall => &["--",],
            Dockerfile => &["#",],
            DotNetResource => &[],
            DreamMaker => &["//",],
            Dust => &[],
            Ebuild => &["#",],
            EdgeQL => &["#",],
            ESDL => &["#",],
            Edn => &[";",],
            Eighth => &["\\ ","-- ",],
            Elisp => &[";",],
            Elixir => &["#",],
            Elm => &["--",],
            Elvish => &["#",],
            EmacsDevEnv => &[";",],
            Emojicode => &["💭",],
            Erlang => &["%",],
            Factor => &["!","#!",],
            FEN => &[],
            Fennel => &[";",";;",],
            Fish => &["#",],
            FlatBuffers => &["//",],
            ForgeConfig => &["#","~",],
            Forth => &["\\",],
            FortranLegacy => &["c","C","!","*",],
            FortranModern => &["!",],
            FreeMarker => &[],
            FSharp => &["//",],
            Fstar => &["//",],
            Futhark => &["--",],
            GDB => &["#",],
            GdScript => &["#",],
            Gherkin => &["#",],
            Gleam => &["//","///","////",],
            GlimmerJs => &["//",],
            GlimmerTs => &["//",],
            Glsl => &["//",],
            Gml => &["//",],
            Go => &["//",],
            Gohtml => &[],
            Graphql => &["#",],
            Groovy => &["//",],
            Gwion => &["#!",],
            Haml => &["-#",],
            Hamlet => &[],
            Happy => &[],
            Handlebars => &[],
            Haskell => &["--",],
            Haxe => &["//",],
            Hcl => &["#","//",],
            Headache => &["//",],
            Hex => &[],
            Hex0 => &["#",";",],
            Hex1 => &["#",";",],
            Hex2 => &["#",";",],
            HiCad => &["REM","rem",],
            Hlsl => &["//",],
            HolyC => &["//",],
            Html => &[],
            Hy => &[";",],
            Idris => &["--",],
            Ini => &[";","#",],
            IntelHex => &[],
            Isabelle => &["--",],
            Jai => &["//",],
            Janet => &["#",],
            Java => &["//",],
            JavaScript => &["//",],
            Jinja2 => &[],
            Jq => &["#",],
            JSLT => &["//",],
            Json => &[],
            Jsonnet => &["//","#",],
            Jsx => &["//",],
            Julia => &["#",],
            Julius => &["//",],
            Jupyter => &[],
            Just => &["#",],
            K => &["/",],
            KakouneScript => &["#",],
            Kaem => &["#",],
            Koka => &["//",],
            Kotlin => &["//",],
            Ksh => &["#",],
            Lalrpop => &["//",],
            KvLanguage => &["# ",],
            Lean => &["--",],
            Hledger => &[";","#",],
            Less => &["//",],
            Lex => &["//",],
            Liquid => &[],
            LinguaFranca => &["//","#",],
            LinkerScript => &[],
            Lisp => &[";",],
            LiveScript => &["#",],
            LLVM => &[";",],
            Logtalk => &["%",],
            LolCode => &["BTW",],
            Lua => &["--",],
            Lucius => &["//",],
            M1Assembly => &["#",";",],
            M4 => &["#","dnl",],
            Madlang => &["#",],
            Makefile => &["#",],
            Markdown => &[],
            Max => &[],
            Mdx => &[],
            Menhir => &["//",],
            Meson => &["#",],
            Metal => &["//",],
            Mint => &[],
            Mlatu => &["//",],
            Modelica => &["//",],
            ModuleDef => &[";",],
            Mojo => &["#",],
            MonkeyC => &["//",],
            MoonBit => &["//",],
            MoonScript => &["--",],
            MsBuild => &[],
            Mustache => &[],
            Nextflow => &["//",],
            Nim => &["#",],
            Nix => &["#",],
            NotQuitePerl => &["#",],
            NuGetConfig => &[],
            Nushell => &["#",],
            ObjectiveC => &["//",],
            ObjectiveCpp => &["//",],
            OCaml => &[],
            Odin => &["//",],
            OpenScad => &["//",],
            OpenPolicyAgent => &["#",],
            OpenCL => &[],
            OpenQasm => &["//",],
            OpenType => &["#",],
            Org => &["# ",],
            Oz => &["%",],
            PacmanMakepkg => &["#",],
            Pan => &["#",],
            Pascal => &["//",],
            Perl => &["#",],
            Pest => &["//",],
            Phix => &["--","//","#!",],
            Php => &["#","//",],
            PlantUml => &["'",],
            Po => &["#",],
            Poke => &[],
            Polly => &[],
            Pony => &["//",],
            PostCss => &["//",],
            PowerShell => &["#",],
            PRACTICE => &[";","//",],
            Processing => &["//",],
            Prolog => &["%",],
            PSL => &["//",],
            Protobuf => &["//",],
            Pug => &["//","//-",],
            Puppet => &["#",],
            PureScript => &["--",],
            Pyret => &["#",],
            Python => &["#",],
            PRQL => &["#",],
            Q => &["/",],
            Qcl => &["//",],
            Qml => &["//",],
            R => &["#",],
            Racket => &[";",],
            Rakefile => &["#",],
            Raku => &["#",],
            Razor => &["//",],
            Redscript => &["//","///",],
            Renpy => &["#",],
            ReScript => &["//",],
            ReStructuredText => &[],
            Roc => &["#",],
            RON => &["//",],
            RPMSpecfile => &["#",],
            Ruby => &["#",],
            RubyHtml => &[],
            Rust => &["//",],
            Sass => &["//",],
            Scala => &["//",],
            Scheme => &[";",],
            Scons => &["#",],
            Sh => &["#",],
            ShaderLab => &["//",],
            SIL => &["//",],
            Slang => &["//",],
            Sml => &[],
            Smalltalk => &[],
            Snakemake => &["#",],
            Solidity => &["//",],
            SpecmanE => &["--","//",],
            Spice => &["*",],
            Sql => &["--",],
            Sqf => &["//",],
            SRecode => &[";;",],
            Stan => &["//","#",],
            Stata => &["//","*",],
            Stratego => &["//",],
            Stylus => &["//",],
            Svelte => &[],
            Svg => &[],
            Swift => &["//",],
            Swig => &["//",],
            SystemVerilog => &["//",],
            Slint => &["//",],
            Tact => &["//",],
            Tcl => &["#",],
            Tera => &[],
            Templ => &["//",],
            Tex => &["%",],
            Text => &[],
            Thrift => &["#","//",],
            Toml => &["#",],
            Tsx => &["//",],
            Ttcn => &["//",],
            Twig => &[],
            TypeScript => &["//",],
            Typst => &["//",],
            Uiua => &["#",],
            UMPL => &["!",],
            Unison => &["--",],
            UnrealDeveloperMarkdown => &[],
            UnrealPlugin => &[],
            UnrealProject => &[],
            UnrealScript => &["//",],
            UnrealShader => &["//",],
            UnrealShaderHeader => &["//",],
            UrWeb => &[],
            UrWebProject => &["#",],
            Vala => &["//",],
            VB6 => &["'",],
            VBScript => &["'","REM",],
            Velocity => &["##",],
            Verilog => &["//",],
            VerilogArgsFile => &[],
            Vhdl => &["--",],
            Virgil => &["//",],
            VisualBasic => &["'",],
            VisualStudioProject => &[],
            VisualStudioSolution => &[],
            VimScript => &["\"",],
            Vue => &["//",],
            WebAssembly => &[";;",],
            WenYan => &[],
            WGSL => &["//",],
            Wolfram => &[],
            Xaml => &[],
            XcodeConfig => &["//",],
            Xml => &[],
            XSL => &[],
            Xtend => &["//",],
            Yaml => &["#",],
            ZenCode => &["//","#",],
            Zig => &["//",],
            Zokrates => &["//",],
            Zsh => &["#",],
            GdShader => &["//",],
            
        }
    }

    /// Returns the single line comments of a language.
    /// ```
    /// use tokei::LanguageType;
    /// let lang = LanguageType::Rust;
    /// assert_eq!(lang.multi_line_comments(), &[("/*", "*/")]);
    /// ```
    pub fn multi_line_comments(self) -> &'static [(&'static str, &'static str)]
    {
        match self {
            Abap => &[],
            ABNF => &[],
            ActionScript => &[("/*","*/",),],
            Ada => &[],
            Agda => &[("{-","-}",),],
            Alex => &[],
            Alloy => &[("/*","*/",),],
            Apl => &[],
            Arduino => &[("/*","*/",),],
            ArkTS => &[("/*","*/",),],
            Arturo => &[],
            AsciiDoc => &[("////","////",),],
            Asn1 => &[("/*","*/",),],
            Asp => &[],
            AspNet => &[("<!--","-->",),("<%--","-->",),],
            Assembly => &[],
            AssemblyGAS => &[("/*","*/",),],
            Astro => &[("/*","*/",),("<!--","-->",),],
            Ats => &[("(*","*)",),("/*","*/",),],
            Autoconf => &[],
            Autoit => &[("#comments-start","#comments-end",),("#cs","#ce",),],
            AutoHotKey => &[("/*","*/",),],
            Automake => &[],
            AvaloniaXaml => &[("<!--","-->",),],
            AWK => &[],
            Ballerina => &[],
            Bash => &[],
            Batch => &[],
            Bazel => &[],
            Bean => &[],
            Bicep => &[("/*","*/",),],
            Bitbake => &[],
            Bqn => &[],
            BrightScript => &[],
            C => &[("/*","*/",),],
            Cabal => &[("{-","-}",),],
            Cairo => &[],
            Cangjie => &[("/*","*/",),],
            Cassius => &[("/*","*/",),],
            Ceylon => &[("/*","*/",),],
            Chapel => &[("/*","*/",),],
            CHeader => &[("/*","*/",),],
            Cil => &[],
            Circom => &[("/*","*/",),],
            Clojure => &[],
            ClojureC => &[],
            ClojureScript => &[],
            CMake => &[],
            Cobol => &[],
            CodeQL => &[("/*","*/",),],
            CoffeeScript => &[("###","###",),],
            Cogent => &[],
            ColdFusion => &[("<!---","--->",),],
            ColdFusionScript => &[("/*","*/",),],
            Coq => &[("(*","*)",),],
            Cpp => &[("/*","*/",),],
            CppHeader => &[("/*","*/",),],
            CppModule => &[("/*","*/",),],
            Crystal => &[],
            CSharp => &[("/*","*/",),],
            CShell => &[],
            Css => &[("/*","*/",),],
            Cuda => &[("/*","*/",),],
            Cue => &[],
            Cython => &[],
            D => &[("/*","*/",),],
            D2 => &[("\"\"\"","\"\"\"",),],
            Daml => &[("{-","-}",),],
            Dart => &[("/*","*/",),],
            DeviceTree => &[("/*","*/",),],
            Dhall => &[("{-","-}",),],
            Dockerfile => &[],
            DotNetResource => &[("<!--","-->",),],
            DreamMaker => &[("/*","*/",),],
            Dust => &[("{!","!}",),],
            Ebuild => &[],
            EdgeQL => &[],
            ESDL => &[],
            Edn => &[],
            Eighth => &[("(*","*)",),],
            Elisp => &[],
            Elixir => &[],
            Elm => &[("{-","-}",),],
            Elvish => &[],
            EmacsDevEnv => &[],
            Emojicode => &[("💭🔜","🔚💭",),("📗","📗",),("📘","📘",),],
            Erlang => &[],
            Factor => &[("/*","*/",),],
            FEN => &[],
            Fennel => &[],
            Fish => &[],
            FlatBuffers => &[("/*","*/",),],
            ForgeConfig => &[],
            Forth => &[("( ",")",),],
            FortranLegacy => &[],
            FortranModern => &[],
            FreeMarker => &[("<#--","-->",),],
            FSharp => &[("(*","*)",),],
            Fstar => &[("(*","*)",),],
            Futhark => &[],
            GDB => &[],
            GdScript => &[],
            Gherkin => &[],
            Gleam => &[],
            GlimmerJs => &[("/*","*/",),("<!--","-->",),],
            GlimmerTs => &[("/*","*/",),("<!--","-->",),],
            Glsl => &[("/*","*/",),],
            Gml => &[("/*","*/",),],
            Go => &[("/*","*/",),],
            Gohtml => &[("<!--","-->",),("{{/*","*/}}",),],
            Graphql => &[],
            Groovy => &[("/*","*/",),],
            Gwion => &[],
            Haml => &[],
            Hamlet => &[("<!--","-->",),],
            Happy => &[],
            Handlebars => &[("<!--","-->",),("{{!","}}",),],
            Haskell => &[("{-","-}",),],
            Haxe => &[("/*","*/",),],
            Hcl => &[("/*","*/",),],
            Headache => &[("/*","*/",),],
            Hex => &[],
            Hex0 => &[],
            Hex1 => &[],
            Hex2 => &[],
            HiCad => &[],
            Hlsl => &[("/*","*/",),],
            HolyC => &[("/*","*/",),],
            Html => &[("<!--","-->",),],
            Hy => &[],
            Idris => &[("{-","-}",),],
            Ini => &[],
            IntelHex => &[],
            Isabelle => &[("{*","*}",),("(*","*)",),("‹","›",),("\\<open>","\\<close>",),],
            Jai => &[("/*","*/",),],
            Janet => &[],
            Java => &[("/*","*/",),],
            JavaScript => &[("/*","*/",),],
            Jinja2 => &[("{#","#}",),],
            Jq => &[],
            JSLT => &[],
            Json => &[],
            Jsonnet => &[("/*","*/",),],
            Jsx => &[("/*","*/",),],
            Julia => &[("#=","=#",),],
            Julius => &[("/*","*/",),],
            Jupyter => &[],
            Just => &[],
            K => &[],
            KakouneScript => &[],
            Kaem => &[],
            Koka => &[("/*","*/",),],
            Kotlin => &[("/*","*/",),],
            Ksh => &[],
            Lalrpop => &[],
            KvLanguage => &[],
            Lean => &[("/-","-/",),],
            Hledger => &[("comment","end comment",),],
            Less => &[("/*","*/",),],
            Lex => &[("/*","*/",),],
            Liquid => &[("<!--","-->",),("{% comment %}","{% endcomment %}",),],
            LinguaFranca => &[("/*","*/",),],
            LinkerScript => &[("/*","*/",),],
            Lisp => &[("#|","|#",),],
            LiveScript => &[("/*","*/",),],
            LLVM => &[],
            Logtalk => &[("/*","*/",),],
            LolCode => &[("OBTW","TLDR",),],
            Lua => &[("--[[","]]",),],
            Lucius => &[("/*","*/",),],
            M1Assembly => &[],
            M4 => &[],
            Madlang => &[("{#","#}",),],
            Makefile => &[],
            Markdown => &[],
            Max => &[],
            Mdx => &[],
            Menhir => &[("(*","*)",),("/*","*/",),],
            Meson => &[],
            Metal => &[("/*","*/",),],
            Mint => &[],
            Mlatu => &[],
            Modelica => &[("/*","*/",),],
            ModuleDef => &[],
            Mojo => &[],
            MonkeyC => &[("/*","*/",),],
            MoonBit => &[],
            MoonScript => &[],
            MsBuild => &[("<!--","-->",),],
            Mustache => &[("{{!","}}",),],
            Nextflow => &[("/*","*/",),],
            Nim => &[],
            Nix => &[("/*","*/",),],
            NotQuitePerl => &[("=begin","=end",),],
            NuGetConfig => &[("<!--","-->",),],
            Nushell => &[],
            ObjectiveC => &[("/*","*/",),],
            ObjectiveCpp => &[("/*","*/",),],
            OCaml => &[("(*","*)",),],
            Odin => &[("/*","*/",),],
            OpenScad => &[("/*","*/",),],
            OpenPolicyAgent => &[],
            OpenCL => &[("/*","*/",),],
            OpenQasm => &[("/*","*/",),],
            OpenType => &[],
            Org => &[],
            Oz => &[("/*","*/",),],
            PacmanMakepkg => &[],
            Pan => &[],
            Pascal => &[("{","}",),("(*","*)",),],
            Perl => &[("=pod","=cut",),],
            Pest => &[],
            Phix => &[("/*","*/",),("--/*","--*/",),],
            Php => &[("/*","*/",),],
            PlantUml => &[("/'","'/",),],
            Po => &[],
            Poke => &[("/*","*/",),],
            Polly => &[("<!--","-->",),],
            Pony => &[("/*","*/",),],
            PostCss => &[("/*","*/",),],
            PowerShell => &[("<#","#>",),],
            PRACTICE => &[],
            Processing => &[("/*","*/",),],
            Prolog => &[("/*","*/",),],
            PSL => &[("/*","*/",),],
            Protobuf => &[],
            Pug => &[],
            Puppet => &[],
            PureScript => &[("{-","-}",),],
            Pyret => &[("#|","|#",),],
            Python => &[],
            PRQL => &[],
            Q => &[],
            Qcl => &[("/*","*/",),],
            Qml => &[("/*","*/",),],
            R => &[],
            Racket => &[("#|","|#",),],
            Rakefile => &[("=begin","=end",),],
            Raku => &[("#`(",")",),("#`[","]",),("#`{","}",),("#`｢","｣",),],
            Razor => &[("<!--","-->",),("@*","*@",),("/*","*/",),],
            Redscript => &[("/*","*/",),],
            Renpy => &[],
            ReScript => &[("/*","*/",),],
            ReStructuredText => &[],
            Roc => &[],
            RON => &[("/*","*/",),],
            RPMSpecfile => &[],
            Ruby => &[("=begin","=end",),],
            RubyHtml => &[("<!--","-->",),],
            Rust => &[("/*","*/",),],
            Sass => &[("/*","*/",),],
            Scala => &[("/*","*/",),],
            Scheme => &[("#|","|#",),],
            Scons => &[],
            Sh => &[],
            ShaderLab => &[("/*","*/",),],
            SIL => &[("/*","*/",),("/+","+/",),],
            Slang => &[("/*","*/",),],
            Sml => &[("(*","*)",),],
            Smalltalk => &[("\"","\"",),],
            Snakemake => &[],
            Solidity => &[("/*","*/",),],
            SpecmanE => &[("'>","<'",),],
            Spice => &[],
            Sql => &[("/*","*/",),],
            Sqf => &[("/*","*/",),],
            SRecode => &[],
            Stan => &[("/*","*/",),],
            Stata => &[("/*","*/",),],
            Stratego => &[("/*","*/",),],
            Stylus => &[("/*","*/",),],
            Svelte => &[("<!--","-->",),],
            Svg => &[("<!--","-->",),],
            Swift => &[("/*","*/",),],
            Swig => &[("/*","*/",),],
            SystemVerilog => &[("/*","*/",),],
            Slint => &[("/*","*/",),],
            Tact => &[("/*","*/",),],
            Tcl => &[],
            Tera => &[("<!--","-->",),("{#","#}",),],
            Templ => &[("<!--","-->",),("/*","*/",),],
            Tex => &[],
            Text => &[],
            Thrift => &[("/*","*/",),],
            Toml => &[],
            Tsx => &[("/*","*/",),],
            Ttcn => &[("/*","*/",),],
            Twig => &[("<!--","-->",),("{#","#}",),],
            TypeScript => &[("/*","*/",),],
            Typst => &[("/*","*/",),],
            Uiua => &[],
            UMPL => &[],
            Unison => &[("{-","-}",),],
            UnrealDeveloperMarkdown => &[],
            UnrealPlugin => &[],
            UnrealProject => &[],
            UnrealScript => &[("/*","*/",),],
            UnrealShader => &[("/*","*/",),],
            UnrealShaderHeader => &[("/*","*/",),],
            UrWeb => &[("(*","*)",),],
            UrWebProject => &[],
            Vala => &[("/*","*/",),],
            VB6 => &[],
            VBScript => &[],
            Velocity => &[("#*","*#",),],
            Verilog => &[("/*","*/",),],
            VerilogArgsFile => &[],
            Vhdl => &[("/*","*/",),],
            Virgil => &[("/*","*/",),],
            VisualBasic => &[],
            VisualStudioProject => &[("<!--","-->",),],
            VisualStudioSolution => &[],
            VimScript => &[],
            Vue => &[("<!--","-->",),("/*","*/",),],
            WebAssembly => &[],
            WenYan => &[("批曰。","。",),("疏曰。","。",),],
            WGSL => &[],
            Wolfram => &[("(*","*)",),],
            Xaml => &[("<!--","-->",),],
            XcodeConfig => &[],
            Xml => &[("<!--","-->",),],
            XSL => &[("<!--","-->",),],
            Xtend => &[("/*","*/",),],
            Yaml => &[],
            ZenCode => &[("/*","*/",),],
            Zig => &[],
            Zokrates => &[("/*","*/",),],
            Zsh => &[],
            GdShader => &[("/*","*/",),],
            
        }
    }


    /// Returns whether the language allows nested multi line comments.
    /// ```
    /// use tokei::LanguageType;
    /// let lang = LanguageType::Rust;
    /// assert!(lang.allows_nested());
    /// ```
    pub fn allows_nested(self) -> bool {
        match self {
            Abap => false,
            ABNF => false,
            ActionScript => false,
            Ada => false,
            Agda => true,
            Alex => false,
            Alloy => false,
            Apl => false,
            Arduino => false,
            ArkTS => false,
            Arturo => false,
            AsciiDoc => false,
            Asn1 => false,
            Asp => false,
            AspNet => false,
            Assembly => false,
            AssemblyGAS => false,
            Astro => false,
            Ats => false,
            Autoconf => false,
            Autoit => false,
            AutoHotKey => false,
            Automake => false,
            AvaloniaXaml => false,
            AWK => false,
            Ballerina => false,
            Bash => false,
            Batch => false,
            Bazel => false,
            Bean => false,
            Bicep => false,
            Bitbake => false,
            Bqn => false,
            BrightScript => false,
            C => false,
            Cabal => true,
            Cairo => false,
            Cangjie => true,
            Cassius => false,
            Ceylon => false,
            Chapel => false,
            CHeader => false,
            Cil => false,
            Circom => false,
            Clojure => false,
            ClojureC => false,
            ClojureScript => false,
            CMake => false,
            Cobol => false,
            CodeQL => false,
            CoffeeScript => false,
            Cogent => false,
            ColdFusion => false,
            ColdFusionScript => false,
            Coq => false,
            Cpp => false,
            CppHeader => false,
            CppModule => false,
            Crystal => false,
            CSharp => false,
            CShell => false,
            Css => false,
            Cuda => false,
            Cue => false,
            Cython => false,
            D => false,
            D2 => false,
            Daml => true,
            Dart => false,
            DeviceTree => false,
            Dhall => true,
            Dockerfile => false,
            DotNetResource => false,
            DreamMaker => true,
            Dust => false,
            Ebuild => false,
            EdgeQL => false,
            ESDL => false,
            Edn => false,
            Eighth => true,
            Elisp => false,
            Elixir => false,
            Elm => true,
            Elvish => false,
            EmacsDevEnv => false,
            Emojicode => false,
            Erlang => false,
            Factor => false,
            FEN => false,
            Fennel => false,
            Fish => false,
            FlatBuffers => false,
            ForgeConfig => false,
            Forth => false,
            FortranLegacy => false,
            FortranModern => false,
            FreeMarker => false,
            FSharp => false,
            Fstar => false,
            Futhark => false,
            GDB => false,
            GdScript => false,
            Gherkin => false,
            Gleam => false,
            GlimmerJs => false,
            GlimmerTs => false,
            Glsl => false,
            Gml => false,
            Go => false,
            Gohtml => false,
            Graphql => false,
            Groovy => false,
            Gwion => false,
            Haml => false,
            Hamlet => false,
            Happy => false,
            Handlebars => false,
            Haskell => true,
            Haxe => false,
            Hcl => false,
            Headache => false,
            Hex => false,
            Hex0 => false,
            Hex1 => false,
            Hex2 => false,
            HiCad => false,
            Hlsl => false,
            HolyC => false,
            Html => false,
            Hy => false,
            Idris => true,
            Ini => false,
            IntelHex => false,
            Isabelle => false,
            Jai => true,
            Janet => false,
            Java => false,
            JavaScript => false,
            Jinja2 => false,
            Jq => false,
            JSLT => false,
            Json => false,
            Jsonnet => false,
            Jsx => false,
            Julia => true,
            Julius => false,
            Jupyter => false,
            Just => false,
            K => true,
            KakouneScript => false,
            Kaem => false,
            Koka => true,
            Kotlin => true,
            Ksh => false,
            Lalrpop => false,
            KvLanguage => false,
            Lean => true,
            Hledger => false,
            Less => false,
            Lex => false,
            Liquid => false,
            LinguaFranca => true,
            LinkerScript => false,
            Lisp => true,
            LiveScript => false,
            LLVM => false,
            Logtalk => false,
            LolCode => false,
            Lua => false,
            Lucius => false,
            M1Assembly => false,
            M4 => false,
            Madlang => false,
            Makefile => false,
            Markdown => false,
            Max => false,
            Mdx => false,
            Menhir => true,
            Meson => false,
            Metal => false,
            Mint => false,
            Mlatu => false,
            Modelica => false,
            ModuleDef => false,
            Mojo => false,
            MonkeyC => false,
            MoonBit => false,
            MoonScript => false,
            MsBuild => false,
            Mustache => false,
            Nextflow => false,
            Nim => false,
            Nix => false,
            NotQuitePerl => false,
            NuGetConfig => false,
            Nushell => false,
            ObjectiveC => false,
            ObjectiveCpp => false,
            OCaml => false,
            Odin => false,
            OpenScad => false,
            OpenPolicyAgent => false,
            OpenCL => false,
            OpenQasm => false,
            OpenType => false,
            Org => false,
            Oz => false,
            PacmanMakepkg => false,
            Pan => false,
            Pascal => true,
            Perl => false,
            Pest => false,
            Phix => true,
            Php => false,
            PlantUml => false,
            Po => false,
            Poke => false,
            Polly => false,
            Pony => false,
            PostCss => false,
            PowerShell => false,
            PRACTICE => false,
            Processing => false,
            Prolog => false,
            PSL => false,
            Protobuf => false,
            Pug => false,
            Puppet => false,
            PureScript => true,
            Pyret => true,
            Python => false,
            PRQL => false,
            Q => true,
            Qcl => false,
            Qml => false,
            R => false,
            Racket => true,
            Rakefile => false,
            Raku => true,
            Razor => false,
            Redscript => true,
            Renpy => false,
            ReScript => false,
            ReStructuredText => false,
            Roc => false,
            RON => true,
            RPMSpecfile => false,
            Ruby => false,
            RubyHtml => false,
            Rust => true,
            Sass => false,
            Scala => false,
            Scheme => true,
            Scons => false,
            Sh => false,
            ShaderLab => false,
            SIL => false,
            Slang => false,
            Sml => false,
            Smalltalk => false,
            Snakemake => false,
            Solidity => false,
            SpecmanE => false,
            Spice => false,
            Sql => false,
            Sqf => false,
            SRecode => false,
            Stan => false,
            Stata => false,
            Stratego => false,
            Stylus => false,
            Svelte => false,
            Svg => false,
            Swift => true,
            Swig => true,
            SystemVerilog => false,
            Slint => false,
            Tact => false,
            Tcl => false,
            Tera => false,
            Templ => false,
            Tex => false,
            Text => false,
            Thrift => false,
            Toml => false,
            Tsx => false,
            Ttcn => false,
            Twig => false,
            TypeScript => false,
            Typst => true,
            Uiua => false,
            UMPL => false,
            Unison => true,
            UnrealDeveloperMarkdown => false,
            UnrealPlugin => false,
            UnrealProject => false,
            UnrealScript => false,
            UnrealShader => false,
            UnrealShaderHeader => false,
            UrWeb => false,
            UrWebProject => false,
            Vala => false,
            VB6 => false,
            VBScript => false,
            Velocity => false,
            Verilog => false,
            VerilogArgsFile => false,
            Vhdl => false,
            Virgil => false,
            VisualBasic => false,
            VisualStudioProject => false,
            VisualStudioSolution => false,
            VimScript => false,
            Vue => false,
            WebAssembly => false,
            WenYan => false,
            WGSL => false,
            Wolfram => false,
            Xaml => false,
            XcodeConfig => false,
            Xml => false,
            XSL => false,
            Xtend => false,
            Yaml => false,
            ZenCode => false,
            Zig => false,
            Zokrates => false,
            Zsh => false,
            GdShader => false,
            
        }
    }

    /// Returns what nested comments the language has. (Currently only D has
    /// any of this type.)
    /// ```
    /// use tokei::LanguageType;
    /// let lang = LanguageType::D;
    /// assert_eq!(lang.nested_comments(), &[("/+", "+/")]);
    /// ```
    pub fn nested_comments(self) -> &'static [(&'static str, &'static str)]
    {
        match self {
            Abap => &[],
            ABNF => &[],
            ActionScript => &[],
            Ada => &[],
            Agda => &[],
            Alex => &[],
            Alloy => &[],
            Apl => &[],
            Arduino => &[],
            ArkTS => &[],
            Arturo => &[],
            AsciiDoc => &[],
            Asn1 => &[],
            Asp => &[],
            AspNet => &[],
            Assembly => &[],
            AssemblyGAS => &[],
            Astro => &[],
            Ats => &[],
            Autoconf => &[],
            Autoit => &[],
            AutoHotKey => &[],
            Automake => &[],
            AvaloniaXaml => &[],
            AWK => &[],
            Ballerina => &[],
            Bash => &[],
            Batch => &[],
            Bazel => &[],
            Bean => &[],
            Bicep => &[],
            Bitbake => &[],
            Bqn => &[],
            BrightScript => &[],
            C => &[],
            Cabal => &[],
            Cairo => &[],
            Cangjie => &[],
            Cassius => &[],
            Ceylon => &[],
            Chapel => &[],
            CHeader => &[],
            Cil => &[],
            Circom => &[],
            Clojure => &[],
            ClojureC => &[],
            ClojureScript => &[],
            CMake => &[],
            Cobol => &[],
            CodeQL => &[],
            CoffeeScript => &[],
            Cogent => &[],
            ColdFusion => &[],
            ColdFusionScript => &[],
            Coq => &[],
            Cpp => &[],
            CppHeader => &[],
            CppModule => &[],
            Crystal => &[],
            CSharp => &[],
            CShell => &[],
            Css => &[],
            Cuda => &[],
            Cue => &[],
            Cython => &[],
            D => &[("/+","+/",),],
            D2 => &[],
            Daml => &[],
            Dart => &[],
            DeviceTree => &[],
            Dhall => &[],
            Dockerfile => &[],
            DotNetResource => &[],
            DreamMaker => &[],
            Dust => &[],
            Ebuild => &[],
            EdgeQL => &[],
            ESDL => &[],
            Edn => &[],
            Eighth => &[],
            Elisp => &[],
            Elixir => &[],
            Elm => &[],
            Elvish => &[],
            EmacsDevEnv => &[],
            Emojicode => &[],
            Erlang => &[],
            Factor => &[],
            FEN => &[],
            Fennel => &[],
            Fish => &[],
            FlatBuffers => &[],
            ForgeConfig => &[],
            Forth => &[],
            FortranLegacy => &[],
            FortranModern => &[],
            FreeMarker => &[],
            FSharp => &[],
            Fstar => &[],
            Futhark => &[],
            GDB => &[],
            GdScript => &[],
            Gherkin => &[],
            Gleam => &[],
            GlimmerJs => &[],
            GlimmerTs => &[],
            Glsl => &[],
            Gml => &[],
            Go => &[],
            Gohtml => &[],
            Graphql => &[],
            Groovy => &[],
            Gwion => &[],
            Haml => &[],
            Hamlet => &[],
            Happy => &[],
            Handlebars => &[],
            Haskell => &[],
            Haxe => &[],
            Hcl => &[],
            Headache => &[],
            Hex => &[],
            Hex0 => &[],
            Hex1 => &[],
            Hex2 => &[],
            HiCad => &[],
            Hlsl => &[],
            HolyC => &[],
            Html => &[],
            Hy => &[],
            Idris => &[],
            Ini => &[],
            IntelHex => &[],
            Isabelle => &[],
            Jai => &[],
            Janet => &[],
            Java => &[],
            JavaScript => &[],
            Jinja2 => &[],
            Jq => &[],
            JSLT => &[],
            Json => &[],
            Jsonnet => &[],
            Jsx => &[],
            Julia => &[],
            Julius => &[],
            Jupyter => &[],
            Just => &[],
            K => &[],
            KakouneScript => &[],
            Kaem => &[],
            Koka => &[],
            Kotlin => &[],
            Ksh => &[],
            Lalrpop => &[],
            KvLanguage => &[],
            Lean => &[],
            Hledger => &[],
            Less => &[],
            Lex => &[],
            Liquid => &[],
            LinguaFranca => &[],
            LinkerScript => &[],
            Lisp => &[],
            LiveScript => &[],
            LLVM => &[],
            Logtalk => &[],
            LolCode => &[],
            Lua => &[],
            Lucius => &[],
            M1Assembly => &[],
            M4 => &[],
            Madlang => &[],
            Makefile => &[],
            Markdown => &[],
            Max => &[],
            Mdx => &[],
            Menhir => &[],
            Meson => &[],
            Metal => &[],
            Mint => &[],
            Mlatu => &[],
            Modelica => &[],
            ModuleDef => &[],
            Mojo => &[],
            MonkeyC => &[],
            MoonBit => &[],
            MoonScript => &[],
            MsBuild => &[],
            Mustache => &[],
            Nextflow => &[],
            Nim => &[],
            Nix => &[],
            NotQuitePerl => &[],
            NuGetConfig => &[],
            Nushell => &[],
            ObjectiveC => &[],
            ObjectiveCpp => &[],
            OCaml => &[],
            Odin => &[],
            OpenScad => &[],
            OpenPolicyAgent => &[],
            OpenCL => &[],
            OpenQasm => &[],
            OpenType => &[],
            Org => &[],
            Oz => &[],
            PacmanMakepkg => &[],
            Pan => &[],
            Pascal => &[],
            Perl => &[],
            Pest => &[],
            Phix => &[],
            Php => &[],
            PlantUml => &[],
            Po => &[],
            Poke => &[],
            Polly => &[],
            Pony => &[],
            PostCss => &[],
            PowerShell => &[],
            PRACTICE => &[],
            Processing => &[],
            Prolog => &[],
            PSL => &[],
            Protobuf => &[],
            Pug => &[],
            Puppet => &[],
            PureScript => &[],
            Pyret => &[],
            Python => &[],
            PRQL => &[],
            Q => &[],
            Qcl => &[],
            Qml => &[],
            R => &[],
            Racket => &[],
            Rakefile => &[],
            Raku => &[],
            Razor => &[],
            Redscript => &[],
            Renpy => &[],
            ReScript => &[],
            ReStructuredText => &[],
            Roc => &[],
            RON => &[],
            RPMSpecfile => &[],
            Ruby => &[],
            RubyHtml => &[],
            Rust => &[],
            Sass => &[],
            Scala => &[],
            Scheme => &[],
            Scons => &[],
            Sh => &[],
            ShaderLab => &[],
            SIL => &[],
            Slang => &[],
            Sml => &[],
            Smalltalk => &[],
            Snakemake => &[],
            Solidity => &[],
            SpecmanE => &[],
            Spice => &[],
            Sql => &[],
            Sqf => &[],
            SRecode => &[],
            Stan => &[],
            Stata => &[],
            Stratego => &[],
            Stylus => &[],
            Svelte => &[],
            Svg => &[],
            Swift => &[],
            Swig => &[],
            SystemVerilog => &[],
            Slint => &[],
            Tact => &[],
            Tcl => &[],
            Tera => &[],
            Templ => &[],
            Tex => &[],
            Text => &[],
            Thrift => &[],
            Toml => &[],
            Tsx => &[],
            Ttcn => &[],
            Twig => &[],
            TypeScript => &[],
            Typst => &[],
            Uiua => &[],
            UMPL => &[],
            Unison => &[],
            UnrealDeveloperMarkdown => &[],
            UnrealPlugin => &[],
            UnrealProject => &[],
            UnrealScript => &[],
            UnrealShader => &[],
            UnrealShaderHeader => &[],
            UrWeb => &[],
            UrWebProject => &[],
            Vala => &[],
            VB6 => &[],
            VBScript => &[],
            Velocity => &[],
            Verilog => &[],
            VerilogArgsFile => &[],
            Vhdl => &[],
            Virgil => &[],
            VisualBasic => &[],
            VisualStudioProject => &[],
            VisualStudioSolution => &[],
            VimScript => &[],
            Vue => &[],
            WebAssembly => &[],
            WenYan => &[],
            WGSL => &[],
            Wolfram => &[],
            Xaml => &[],
            XcodeConfig => &[],
            Xml => &[],
            XSL => &[],
            Xtend => &[],
            Yaml => &[],
            ZenCode => &[],
            Zig => &[],
            Zokrates => &[],
            Zsh => &[],
            GdShader => &[],
            
        }
    }

    /// Returns the quotes of a language.
    /// ```
    /// use tokei::LanguageType;
    /// let lang = LanguageType::C;
    /// assert_eq!(lang.quotes(), &[("\"", "\"")]);
    /// ```
    pub fn quotes(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Abap => &[],
            ABNF => &[],
            ActionScript => &[("\"","\"",),],
            Ada => &[],
            Agda => &[],
            Alex => &[],
            Alloy => &[],
            Apl => &[("'","'",),],
            Arduino => &[("\"","\"",),],
            ArkTS => &[("\"","\"",),("'","'",),("`","`",),],
            Arturo => &[("\"","\"",),],
            AsciiDoc => &[],
            Asn1 => &[("\"","\"",),("'","'",),],
            Asp => &[],
            AspNet => &[],
            Assembly => &[("\"","\"",),("'","'",),],
            AssemblyGAS => &[("\"","\"",),],
            Astro => &[],
            Ats => &[("\"","\"",),],
            Autoconf => &[],
            Autoit => &[],
            AutoHotKey => &[],
            Automake => &[],
            AvaloniaXaml => &[("\"","\"",),("'","'",),],
            AWK => &[],
            Ballerina => &[("\"","\"",),("`","`",),],
            Bash => &[("\"","\"",),("'","'",),],
            Batch => &[],
            Bazel => &[("\"","\"",),("'","'",),],
            Bean => &[("\"","\"",),],
            Bicep => &[("'''","'''",),("'","'",),],
            Bitbake => &[("\"","\"",),("'","'",),],
            Bqn => &[("\"","\"",),("'","'",),],
            BrightScript => &[("\"","\"",),],
            C => &[("\"","\"",),],
            Cabal => &[],
            Cairo => &[("\"","\"",),("'","'",),],
            Cangjie => &[("\"\"\"","\"\"\"",),("\"","\"",),],
            Cassius => &[("\"","\"",),("'","'",),],
            Ceylon => &[("\"\"\"","\"\"\"",),("\"","\"",),],
            Chapel => &[("\"","\"",),("'","'",),],
            CHeader => &[("\"","\"",),],
            Cil => &[("\"","\"",),],
            Circom => &[],
            Clojure => &[("\"","\"",),],
            ClojureC => &[("\"","\"",),],
            ClojureScript => &[("\"","\"",),],
            CMake => &[("\"","\"",),],
            Cobol => &[],
            CodeQL => &[("\"","\"",),],
            CoffeeScript => &[("\"","\"",),("'","'",),],
            Cogent => &[],
            ColdFusion => &[("\"","\"",),("'","'",),],
            ColdFusionScript => &[("\"","\"",),],
            Coq => &[("\"","\"",),],
            Cpp => &[("\"","\"",),],
            CppHeader => &[("\"","\"",),],
            CppModule => &[("\"","\"",),],
            Crystal => &[("\"","\"",),("'","'",),],
            CSharp => &[("\"","\"",),],
            CShell => &[],
            Css => &[("\"","\"",),("'","'",),],
            Cuda => &[("\"","\"",),],
            Cue => &[("\"\"\"","\"\"\"",),("\"","\"",),("'","'",),],
            Cython => &[("\"","\"",),("'","'",),],
            D => &[("\"","\"",),("'","'",),],
            D2 => &[],
            Daml => &[],
            Dart => &[("\"\"\"","\"\"\"",),("'''","'''",),("\"","\"",),("'","'",),],
            DeviceTree => &[("\"","\"",),],
            Dhall => &[("\"","\"",),("''","''",),],
            Dockerfile => &[("\"","\"",),("'","'",),],
            DotNetResource => &[("\"","\"",),],
            DreamMaker => &[("{\"","\"}",),("\"","\"",),("'","'",),],
            Dust => &[],
            Ebuild => &[("\"","\"",),("'","'",),],
            EdgeQL => &[("\"","\"",),("'","'",),("$","$",),],
            ESDL => &[("\"","\"",),("'","'",),],
            Edn => &[],
            Eighth => &[("\"","\"",),],
            Elisp => &[],
            Elixir => &[("\"\"\"","\"\"\"",),("'''","'''",),("\"","\"",),("'","'",),],
            Elm => &[],
            Elvish => &[("\"","\"",),("'","'",),],
            EmacsDevEnv => &[],
            Emojicode => &[("❌🔤","❌🔤",),],
            Erlang => &[],
            Factor => &[],
            FEN => &[],
            Fennel => &[("\"","\"",),],
            Fish => &[("\"","\"",),("'","'",),],
            FlatBuffers => &[("\"","\"",),],
            ForgeConfig => &[],
            Forth => &[],
            FortranLegacy => &[("\"","\"",),("'","'",),],
            FortranModern => &[("\"","\"",),],
            FreeMarker => &[],
            FSharp => &[("\"","\"",),],
            Fstar => &[("\"","\"",),],
            Futhark => &[],
            GDB => &[],
            GdScript => &[("\"\"\"","\"\"\"",),("\"","\"",),("'","'",),],
            Gherkin => &[],
            Gleam => &[("\"","\"",),],
            GlimmerJs => &[("\"","\"",),("'","'",),("`","`",),],
            GlimmerTs => &[("\"","\"",),("'","'",),("`","`",),],
            Glsl => &[("\"","\"",),],
            Gml => &[("\"","\"",),],
            Go => &[("\"","\"",),],
            Gohtml => &[("\"","\"",),("'","'",),],
            Graphql => &[("\"\"\"","\"\"\"",),("\"","\"",),],
            Groovy => &[("\"","\"",),],
            Gwion => &[("\"","\"",),],
            Haml => &[("\"","\"",),("'","'",),],
            Hamlet => &[("\"","\"",),("'","'",),],
            Happy => &[],
            Handlebars => &[("\"","\"",),("'","'",),],
            Haskell => &[],
            Haxe => &[("\"","\"",),("'","'",),],
            Hcl => &[("\"","\"",),],
            Headache => &[("\"","\"",),],
            Hex => &[],
            Hex0 => &[],
            Hex1 => &[],
            Hex2 => &[],
            HiCad => &[],
            Hlsl => &[("\"","\"",),],
            HolyC => &[("\"","\"",),],
            Html => &[("\"","\"",),("'","'",),],
            Hy => &[],
            Idris => &[("\"\"\"","\"\"\"",),("\"","\"",),],
            Ini => &[],
            IntelHex => &[],
            Isabelle => &[("''","''",),],
            Jai => &[("\"","\"",),],
            Janet => &[("\"","\"",),("'","'",),("`","`",),],
            Java => &[("\"","\"",),],
            JavaScript => &[("\"","\"",),("'","'",),("`","`",),],
            Jinja2 => &[],
            Jq => &[("\"","\"",),],
            JSLT => &[("\"","\"",),],
            Json => &[],
            Jsonnet => &[("\"","\"",),("'","'",),],
            Jsx => &[("\"","\"",),("'","'",),("`","`",),],
            Julia => &[("\"\"\"","\"\"\"",),("\"","\"",),],
            Julius => &[("\"","\"",),("'","'",),("`","`",),],
            Jupyter => &[],
            Just => &[],
            K => &[("\"","\"",),],
            KakouneScript => &[("\"","\"",),("'","'",),],
            Kaem => &[],
            Koka => &[("\"","\"",),],
            Kotlin => &[("\"\"\"","\"\"\"",),("\"","\"",),],
            Ksh => &[("\"","\"",),("'","'",),],
            Lalrpop => &[("#\"","\"#",),("\"","\"",),],
            KvLanguage => &[("\"","\"",),("'","'",),],
            Lean => &[],
            Hledger => &[],
            Less => &[("\"","\"",),("'","'",),],
            Lex => &[],
            Liquid => &[("\"","\"",),("'","'",),],
            LinguaFranca => &[("\"","\"",),],
            LinkerScript => &[("\"","\"",),],
            Lisp => &[],
            LiveScript => &[("\"","\"",),("'","'",),],
            LLVM => &[("\"","\"",),("'","'",),],
            Logtalk => &[("\"","\"",),],
            LolCode => &[("\"","\"",),],
            Lua => &[("\"","\"",),("'","'",),],
            Lucius => &[("\"","\"",),("'","'",),],
            M1Assembly => &[("\"","\"",),],
            M4 => &[("`","'",),],
            Madlang => &[],
            Makefile => &[],
            Markdown => &[],
            Max => &[],
            Mdx => &[],
            Menhir => &[("\"","\"",),],
            Meson => &[("'''","'''",),("'","'",),],
            Metal => &[("\"","\"",),],
            Mint => &[],
            Mlatu => &[("\"","\"",),],
            Modelica => &[("\"","\"",),],
            ModuleDef => &[],
            Mojo => &[("\"","\"",),("'","'",),],
            MonkeyC => &[("\"","\"",),],
            MoonBit => &[("\"","\"",),],
            MoonScript => &[("\"","\"",),("'","'",),],
            MsBuild => &[("\"","\"",),("'","'",),],
            Mustache => &[("\"","\"",),("'","'",),],
            Nextflow => &[("\"","\"",),],
            Nim => &[("\"\"\"","\"\"\"",),("\"","\"",),],
            Nix => &[("\"","\"",),],
            NotQuitePerl => &[("\"","\"",),("'","'",),],
            NuGetConfig => &[("\"","\"",),("'","'",),],
            Nushell => &[("\"","\"",),("'","'",),],
            ObjectiveC => &[("\"","\"",),],
            ObjectiveCpp => &[("\"","\"",),],
            OCaml => &[("\"","\"",),],
            Odin => &[("\"","\"",),("'","'",),],
            OpenScad => &[("\"","\"",),("'","'",),],
            OpenPolicyAgent => &[("\"","\"",),("`","`",),],
            OpenCL => &[],
            OpenQasm => &[],
            OpenType => &[],
            Org => &[],
            Oz => &[("\"","\"",),],
            PacmanMakepkg => &[("\"","\"",),("'","'",),],
            Pan => &[("\"","\"",),("'","'",),],
            Pascal => &[("'","'",),],
            Perl => &[("\"","\"",),("'","'",),],
            Pest => &[("\"","\"",),("'","'",),],
            Phix => &[("\"","\"",),("'","'",),],
            Php => &[("\"","\"",),("'","'",),],
            PlantUml => &[("\"","\"",),],
            Po => &[],
            Poke => &[],
            Polly => &[("\"","\"",),("'","'",),],
            Pony => &[("\"","\"",),],
            PostCss => &[("\"","\"",),("'","'",),],
            PowerShell => &[("\"@","@\"",),("\"","\"",),("@'","'@",),("'","'",),],
            PRACTICE => &[("\"","\"",),],
            Processing => &[("\"","\"",),],
            Prolog => &[("\"","\"",),],
            PSL => &[("\"","\"",),],
            Protobuf => &[],
            Pug => &[("#{\"","\"}",),("#{'","'}",),("#{`","`}",),],
            Puppet => &[("\"","\"",),("'","'",),],
            PureScript => &[],
            Pyret => &[("\"","\"",),("'","'",),],
            Python => &[("\"","\"",),("'","'",),],
            PRQL => &[("\"","\"",),("'","'",),],
            Q => &[("\"","\"",),],
            Qcl => &[("\"","\"",),],
            Qml => &[("\"","\"",),("'","'",),],
            R => &[],
            Racket => &[],
            Rakefile => &[("\"","\"",),("'","'",),],
            Raku => &[("\"","\"",),("'","'",),],
            Razor => &[("\"","\"",),],
            Redscript => &[("\"","\"",),],
            Renpy => &[("\"","\"",),("'","'",),("`","`",),],
            ReScript => &[("\"","\"",),],
            ReStructuredText => &[],
            Roc => &[("\"","\"",),("'","'",),],
            RON => &[("\"","\"",),],
            RPMSpecfile => &[],
            Ruby => &[("\"","\"",),("'","'",),],
            RubyHtml => &[("\"","\"",),("'","'",),],
            Rust => &[("#\"","\"#",),("\"","\"",),],
            Sass => &[("\"","\"",),("'","'",),],
            Scala => &[("\"","\"",),],
            Scheme => &[],
            Scons => &[("\"\"\"","\"\"\"",),("'''","'''",),("\"","\"",),("'","'",),],
            Sh => &[("\"","\"",),("'","'",),],
            ShaderLab => &[("\"","\"",),],
            SIL => &[("\"","\"",),("'","'",),("`","`",),],
            Slang => &[("\"","\"",),],
            Sml => &[("\"","\"",),],
            Smalltalk => &[("'","'",),],
            Snakemake => &[("\"","\"",),("'","'",),],
            Solidity => &[("\"","\"",),],
            SpecmanE => &[],
            Spice => &[],
            Sql => &[("'","'",),],
            Sqf => &[("\"","\"",),("'","'",),],
            SRecode => &[],
            Stan => &[("\"","\"",),],
            Stata => &[],
            Stratego => &[("\"","\"",),("$[","]",),("$<",">",),("${","}",),],
            Stylus => &[("\"","\"",),("'","'",),],
            Svelte => &[("\"","\"",),("'","'",),],
            Svg => &[("\"","\"",),("'","'",),],
            Swift => &[("\"","\"",),],
            Swig => &[("\"","\"",),],
            SystemVerilog => &[("\"","\"",),],
            Slint => &[("\"","\"",),],
            Tact => &[("\"","\"",),],
            Tcl => &[("\"","\"",),("'","'",),],
            Tera => &[("\"","\"",),("'","'",),],
            Templ => &[("\"","\"",),("'","'",),("`","`",),],
            Tex => &[],
            Text => &[],
            Thrift => &[("\"","\"",),("'","'",),],
            Toml => &[("\"\"\"","\"\"\"",),("'''","'''",),("\"","\"",),("'","'",),],
            Tsx => &[("\"","\"",),("'","'",),("`","`",),],
            Ttcn => &[("\"","\"",),],
            Twig => &[("\"","\"",),("'","'",),],
            TypeScript => &[("\"","\"",),("'","'",),("`","`",),],
            Typst => &[("\"","\"",),],
            Uiua => &[("\"","\"",),],
            UMPL => &[("`","`",),],
            Unison => &[("\"","\"",),],
            UnrealDeveloperMarkdown => &[],
            UnrealPlugin => &[],
            UnrealProject => &[],
            UnrealScript => &[("\"","\"",),],
            UnrealShader => &[("\"","\"",),],
            UnrealShaderHeader => &[("\"","\"",),],
            UrWeb => &[("\"","\"",),],
            UrWebProject => &[],
            Vala => &[("\"","\"",),],
            VB6 => &[],
            VBScript => &[],
            Velocity => &[("\"","\"",),("'","'",),],
            Verilog => &[("\"","\"",),],
            VerilogArgsFile => &[],
            Vhdl => &[],
            Virgil => &[("\"","\"",),],
            VisualBasic => &[("\"","\"",),],
            VisualStudioProject => &[("\"","\"",),("'","'",),],
            VisualStudioSolution => &[],
            VimScript => &[("\"","\"",),("'","'",),],
            Vue => &[("\"","\"",),("'","'",),("`","`",),],
            WebAssembly => &[("\"","\"",),("'","'",),],
            WenYan => &[],
            WGSL => &[],
            Wolfram => &[("\"","\"",),],
            Xaml => &[("\"","\"",),("'","'",),],
            XcodeConfig => &[("\"","\"",),("'","'",),],
            Xml => &[("\"","\"",),("'","'",),],
            XSL => &[("\"","\"",),("'","'",),],
            Xtend => &[("'''","'''",),("\"","\"",),("'","'",),],
            Yaml => &[("\"","\"",),("'","'",),],
            ZenCode => &[("\"","\"",),("'","'",),],
            Zig => &[("\"","\"",),],
            Zokrates => &[],
            Zsh => &[("\"","\"",),("'","'",),],
            GdShader => &[],
            
        }
    }

    /// Returns the verbatim quotes of a language.
    /// ```
    /// use tokei::LanguageType;
    /// let lang = LanguageType::CSharp;
    /// assert_eq!(lang.verbatim_quotes(), &[("@\"", "\"")]);
    /// ```
    pub fn verbatim_quotes(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Abap => &[],
            ABNF => &[],
            ActionScript => &[],
            Ada => &[],
            Agda => &[],
            Alex => &[],
            Alloy => &[],
            Apl => &[],
            Arduino => &[],
            ArkTS => &[],
            Arturo => &[],
            AsciiDoc => &[],
            Asn1 => &[],
            Asp => &[],
            AspNet => &[],
            Assembly => &[],
            AssemblyGAS => &[],
            Astro => &[],
            Ats => &[],
            Autoconf => &[],
            Autoit => &[],
            AutoHotKey => &[],
            Automake => &[],
            AvaloniaXaml => &[],
            AWK => &[],
            Ballerina => &[],
            Bash => &[],
            Batch => &[],
            Bazel => &[],
            Bean => &[],
            Bicep => &[],
            Bitbake => &[],
            Bqn => &[],
            BrightScript => &[],
            C => &[],
            Cabal => &[],
            Cairo => &[],
            Cangjie => &[("###\"","\"###",),("##\"","\"##",),("###'","'###",),("#\"","\"#",),("##'","'##",),("#'","'#",),],
            Cassius => &[],
            Ceylon => &[],
            Chapel => &[],
            CHeader => &[],
            Cil => &[],
            Circom => &[],
            Clojure => &[],
            ClojureC => &[],
            ClojureScript => &[],
            CMake => &[],
            Cobol => &[],
            CodeQL => &[],
            CoffeeScript => &[],
            Cogent => &[],
            ColdFusion => &[],
            ColdFusionScript => &[],
            Coq => &[],
            Cpp => &[("R\"(",")\"",),],
            CppHeader => &[],
            CppModule => &[("R\"(",")\"",),],
            Crystal => &[],
            CSharp => &[("@\"","\"",),],
            CShell => &[],
            Css => &[],
            Cuda => &[],
            Cue => &[("#\"","\"#",),],
            Cython => &[],
            D => &[],
            D2 => &[],
            Daml => &[],
            Dart => &[],
            DeviceTree => &[],
            Dhall => &[],
            Dockerfile => &[],
            DotNetResource => &[],
            DreamMaker => &[],
            Dust => &[],
            Ebuild => &[],
            EdgeQL => &[],
            ESDL => &[],
            Edn => &[],
            Eighth => &[],
            Elisp => &[],
            Elixir => &[],
            Elm => &[],
            Elvish => &[],
            EmacsDevEnv => &[],
            Emojicode => &[],
            Erlang => &[],
            Factor => &[],
            FEN => &[],
            Fennel => &[],
            Fish => &[],
            FlatBuffers => &[],
            ForgeConfig => &[],
            Forth => &[],
            FortranLegacy => &[],
            FortranModern => &[],
            FreeMarker => &[],
            FSharp => &[("@\"","\"",),],
            Fstar => &[],
            Futhark => &[],
            GDB => &[],
            GdScript => &[],
            Gherkin => &[],
            Gleam => &[],
            GlimmerJs => &[],
            GlimmerTs => &[],
            Glsl => &[],
            Gml => &[],
            Go => &[],
            Gohtml => &[],
            Graphql => &[],
            Groovy => &[],
            Gwion => &[],
            Haml => &[],
            Hamlet => &[],
            Happy => &[],
            Handlebars => &[],
            Haskell => &[],
            Haxe => &[],
            Hcl => &[],
            Headache => &[],
            Hex => &[],
            Hex0 => &[],
            Hex1 => &[],
            Hex2 => &[],
            HiCad => &[],
            Hlsl => &[],
            HolyC => &[],
            Html => &[],
            Hy => &[],
            Idris => &[],
            Ini => &[],
            IntelHex => &[],
            Isabelle => &[],
            Jai => &[],
            Janet => &[],
            Java => &[],
            JavaScript => &[],
            Jinja2 => &[],
            Jq => &[],
            JSLT => &[],
            Json => &[],
            Jsonnet => &[],
            Jsx => &[],
            Julia => &[],
            Julius => &[],
            Jupyter => &[],
            Just => &[],
            K => &[],
            KakouneScript => &[],
            Kaem => &[],
            Koka => &[],
            Kotlin => &[],
            Ksh => &[],
            Lalrpop => &[("r##\"","\"##",),("r#\"","\"#",),],
            KvLanguage => &[],
            Lean => &[],
            Hledger => &[],
            Less => &[],
            Lex => &[],
            Liquid => &[],
            LinguaFranca => &[],
            LinkerScript => &[],
            Lisp => &[],
            LiveScript => &[],
            LLVM => &[],
            Logtalk => &[],
            LolCode => &[],
            Lua => &[],
            Lucius => &[],
            M1Assembly => &[],
            M4 => &[],
            Madlang => &[],
            Makefile => &[],
            Markdown => &[],
            Max => &[],
            Mdx => &[],
            Menhir => &[],
            Meson => &[],
            Metal => &[],
            Mint => &[],
            Mlatu => &[],
            Modelica => &[],
            ModuleDef => &[],
            Mojo => &[],
            MonkeyC => &[],
            MoonBit => &[],
            MoonScript => &[],
            MsBuild => &[],
            Mustache => &[],
            Nextflow => &[],
            Nim => &[],
            Nix => &[],
            NotQuitePerl => &[],
            NuGetConfig => &[],
            Nushell => &[],
            ObjectiveC => &[],
            ObjectiveCpp => &[],
            OCaml => &[],
            Odin => &[],
            OpenScad => &[],
            OpenPolicyAgent => &[],
            OpenCL => &[],
            OpenQasm => &[],
            OpenType => &[],
            Org => &[],
            Oz => &[],
            PacmanMakepkg => &[],
            Pan => &[],
            Pascal => &[],
            Perl => &[],
            Pest => &[],
            Phix => &[("\"\"\"","\"\"\"",),("`","`",),],
            Php => &[],
            PlantUml => &[],
            Po => &[],
            Poke => &[],
            Polly => &[],
            Pony => &[],
            PostCss => &[],
            PowerShell => &[],
            PRACTICE => &[],
            Processing => &[],
            Prolog => &[],
            PSL => &[],
            Protobuf => &[],
            Pug => &[],
            Puppet => &[],
            PureScript => &[],
            Pyret => &[],
            Python => &[],
            PRQL => &[],
            Q => &[],
            Qcl => &[],
            Qml => &[],
            R => &[],
            Racket => &[],
            Rakefile => &[],
            Raku => &[("｢","｣",),],
            Razor => &[("@\"","\"",),],
            Redscript => &[],
            Renpy => &[],
            ReScript => &[],
            ReStructuredText => &[],
            Roc => &[],
            RON => &[],
            RPMSpecfile => &[],
            Ruby => &[],
            RubyHtml => &[],
            Rust => &[("r##\"","\"##",),("r#\"","\"#",),],
            Sass => &[],
            Scala => &[],
            Scheme => &[],
            Scons => &[],
            Sh => &[],
            ShaderLab => &[],
            SIL => &[],
            Slang => &[],
            Sml => &[],
            Smalltalk => &[],
            Snakemake => &[],
            Solidity => &[],
            SpecmanE => &[],
            Spice => &[],
            Sql => &[],
            Sqf => &[],
            SRecode => &[],
            Stan => &[],
            Stata => &[],
            Stratego => &[],
            Stylus => &[],
            Svelte => &[],
            Svg => &[],
            Swift => &[],
            Swig => &[],
            SystemVerilog => &[],
            Slint => &[],
            Tact => &[],
            Tcl => &[],
            Tera => &[],
            Templ => &[],
            Tex => &[],
            Text => &[],
            Thrift => &[],
            Toml => &[],
            Tsx => &[],
            Ttcn => &[],
            Twig => &[],
            TypeScript => &[],
            Typst => &[],
            Uiua => &[],
            UMPL => &[],
            Unison => &[],
            UnrealDeveloperMarkdown => &[],
            UnrealPlugin => &[],
            UnrealProject => &[],
            UnrealScript => &[],
            UnrealShader => &[],
            UnrealShaderHeader => &[],
            UrWeb => &[],
            UrWebProject => &[],
            Vala => &[],
            VB6 => &[],
            VBScript => &[],
            Velocity => &[],
            Verilog => &[],
            VerilogArgsFile => &[],
            Vhdl => &[],
            Virgil => &[],
            VisualBasic => &[],
            VisualStudioProject => &[],
            VisualStudioSolution => &[],
            VimScript => &[],
            Vue => &[],
            WebAssembly => &[],
            WenYan => &[],
            WGSL => &[],
            Wolfram => &[],
            Xaml => &[],
            XcodeConfig => &[],
            Xml => &[],
            XSL => &[],
            Xtend => &[],
            Yaml => &[],
            ZenCode => &[("@\"","\"",),("@'","'",),],
            Zig => &[],
            Zokrates => &[],
            Zsh => &[],
            GdShader => &[],
            
        }
    }

    /// Returns the doc quotes of a language.
    /// ```
    /// use tokei::LanguageType;
    /// let lang = LanguageType::Python;
    /// assert_eq!(lang.doc_quotes(), &[("\"\"\"", "\"\"\""), ("'''", "'''")]);
    /// ```
    pub fn doc_quotes(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Abap => &[
                    
                ],ABNF => &[
                    
                ],ActionScript => &[
                    
                ],Ada => &[
                    
                ],Agda => &[
                    
                ],Alex => &[
                    
                ],Alloy => &[
                    
                ],Apl => &[
                    
                ],Arduino => &[
                    
                ],ArkTS => &[
                    
                ],Arturo => &[
                    
                ],AsciiDoc => &[
                    
                ],Asn1 => &[
                    
                ],Asp => &[
                    
                ],AspNet => &[
                    
                ],Assembly => &[
                    
                ],AssemblyGAS => &[
                    
                ],Astro => &[
                    
                ],Ats => &[
                    
                ],Autoconf => &[
                    
                ],Autoit => &[
                    
                ],AutoHotKey => &[
                    
                ],Automake => &[
                    
                ],AvaloniaXaml => &[
                    
                ],AWK => &[
                    
                ],Ballerina => &[
                    
                ],Bash => &[
                    
                ],Batch => &[
                    
                ],Bazel => &[
                    ("\"\"\"","\"\"\"",),("'''","'''",),
                ],Bean => &[
                    
                ],Bicep => &[
                    
                ],Bitbake => &[
                    
                ],Bqn => &[
                    
                ],BrightScript => &[
                    
                ],C => &[
                    
                ],Cabal => &[
                    
                ],Cairo => &[
                    
                ],Cangjie => &[
                    
                ],Cassius => &[
                    
                ],Ceylon => &[
                    
                ],Chapel => &[
                    
                ],CHeader => &[
                    
                ],Cil => &[
                    
                ],Circom => &[
                    
                ],Clojure => &[
                    
                ],ClojureC => &[
                    
                ],ClojureScript => &[
                    
                ],CMake => &[
                    
                ],Cobol => &[
                    
                ],CodeQL => &[
                    
                ],CoffeeScript => &[
                    
                ],Cogent => &[
                    
                ],ColdFusion => &[
                    
                ],ColdFusionScript => &[
                    
                ],Coq => &[
                    
                ],Cpp => &[
                    
                ],CppHeader => &[
                    
                ],CppModule => &[
                    
                ],Crystal => &[
                    
                ],CSharp => &[
                    
                ],CShell => &[
                    
                ],Css => &[
                    
                ],Cuda => &[
                    
                ],Cue => &[
                    
                ],Cython => &[
                    ("\"\"\"","\"\"\"",),("'''","'''",),
                ],D => &[
                    
                ],D2 => &[
                    
                ],Daml => &[
                    
                ],Dart => &[
                    
                ],DeviceTree => &[
                    
                ],Dhall => &[
                    
                ],Dockerfile => &[
                    
                ],DotNetResource => &[
                    
                ],DreamMaker => &[
                    
                ],Dust => &[
                    
                ],Ebuild => &[
                    
                ],EdgeQL => &[
                    
                ],ESDL => &[
                    
                ],Edn => &[
                    
                ],Eighth => &[
                    
                ],Elisp => &[
                    
                ],Elixir => &[
                    
                ],Elm => &[
                    
                ],Elvish => &[
                    
                ],EmacsDevEnv => &[
                    
                ],Emojicode => &[
                    
                ],Erlang => &[
                    
                ],Factor => &[
                    
                ],FEN => &[
                    
                ],Fennel => &[
                    
                ],Fish => &[
                    
                ],FlatBuffers => &[
                    
                ],ForgeConfig => &[
                    
                ],Forth => &[
                    
                ],FortranLegacy => &[
                    
                ],FortranModern => &[
                    
                ],FreeMarker => &[
                    
                ],FSharp => &[
                    
                ],Fstar => &[
                    
                ],Futhark => &[
                    
                ],GDB => &[
                    
                ],GdScript => &[
                    
                ],Gherkin => &[
                    
                ],Gleam => &[
                    
                ],GlimmerJs => &[
                    
                ],GlimmerTs => &[
                    
                ],Glsl => &[
                    
                ],Gml => &[
                    
                ],Go => &[
                    
                ],Gohtml => &[
                    
                ],Graphql => &[
                    
                ],Groovy => &[
                    
                ],Gwion => &[
                    
                ],Haml => &[
                    
                ],Hamlet => &[
                    
                ],Happy => &[
                    
                ],Handlebars => &[
                    
                ],Haskell => &[
                    
                ],Haxe => &[
                    
                ],Hcl => &[
                    
                ],Headache => &[
                    
                ],Hex => &[
                    
                ],Hex0 => &[
                    
                ],Hex1 => &[
                    
                ],Hex2 => &[
                    
                ],HiCad => &[
                    
                ],Hlsl => &[
                    
                ],HolyC => &[
                    
                ],Html => &[
                    
                ],Hy => &[
                    
                ],Idris => &[
                    
                ],Ini => &[
                    
                ],IntelHex => &[
                    
                ],Isabelle => &[
                    
                ],Jai => &[
                    
                ],Janet => &[
                    
                ],Java => &[
                    
                ],JavaScript => &[
                    
                ],Jinja2 => &[
                    
                ],Jq => &[
                    
                ],JSLT => &[
                    
                ],Json => &[
                    
                ],Jsonnet => &[
                    
                ],Jsx => &[
                    
                ],Julia => &[
                    
                ],Julius => &[
                    
                ],Jupyter => &[
                    
                ],Just => &[
                    
                ],K => &[
                    
                ],KakouneScript => &[
                    
                ],Kaem => &[
                    
                ],Koka => &[
                    
                ],Kotlin => &[
                    
                ],Ksh => &[
                    
                ],Lalrpop => &[
                    
                ],KvLanguage => &[
                    ("\"\"\"","\"\"\"",),("'''","'''",),
                ],Lean => &[
                    
                ],Hledger => &[
                    
                ],Less => &[
                    
                ],Lex => &[
                    
                ],Liquid => &[
                    
                ],LinguaFranca => &[
                    
                ],LinkerScript => &[
                    
                ],Lisp => &[
                    
                ],LiveScript => &[
                    
                ],LLVM => &[
                    
                ],Logtalk => &[
                    
                ],LolCode => &[
                    
                ],Lua => &[
                    
                ],Lucius => &[
                    
                ],M1Assembly => &[
                    
                ],M4 => &[
                    
                ],Madlang => &[
                    
                ],Makefile => &[
                    
                ],Markdown => &[
                    
                ],Max => &[
                    
                ],Mdx => &[
                    
                ],Menhir => &[
                    
                ],Meson => &[
                    
                ],Metal => &[
                    
                ],Mint => &[
                    
                ],Mlatu => &[
                    
                ],Modelica => &[
                    
                ],ModuleDef => &[
                    
                ],Mojo => &[
                    ("\"\"\"","\"\"\"",),("'''","'''",),
                ],MonkeyC => &[
                    
                ],MoonBit => &[
                    
                ],MoonScript => &[
                    
                ],MsBuild => &[
                    
                ],Mustache => &[
                    
                ],Nextflow => &[
                    
                ],Nim => &[
                    
                ],Nix => &[
                    
                ],NotQuitePerl => &[
                    
                ],NuGetConfig => &[
                    
                ],Nushell => &[
                    
                ],ObjectiveC => &[
                    
                ],ObjectiveCpp => &[
                    
                ],OCaml => &[
                    
                ],Odin => &[
                    
                ],OpenScad => &[
                    
                ],OpenPolicyAgent => &[
                    
                ],OpenCL => &[
                    
                ],OpenQasm => &[
                    
                ],OpenType => &[
                    
                ],Org => &[
                    
                ],Oz => &[
                    
                ],PacmanMakepkg => &[
                    
                ],Pan => &[
                    
                ],Pascal => &[
                    
                ],Perl => &[
                    
                ],Pest => &[
                    
                ],Phix => &[
                    
                ],Php => &[
                    
                ],PlantUml => &[
                    
                ],Po => &[
                    
                ],Poke => &[
                    
                ],Polly => &[
                    
                ],Pony => &[
                    ("\"\"\"","\"\"\"",),
                ],PostCss => &[
                    
                ],PowerShell => &[
                    
                ],PRACTICE => &[
                    
                ],Processing => &[
                    
                ],Prolog => &[
                    
                ],PSL => &[
                    
                ],Protobuf => &[
                    
                ],Pug => &[
                    
                ],Puppet => &[
                    
                ],PureScript => &[
                    
                ],Pyret => &[
                    
                ],Python => &[
                    ("\"\"\"","\"\"\"",),("'''","'''",),
                ],PRQL => &[
                    
                ],Q => &[
                    
                ],Qcl => &[
                    
                ],Qml => &[
                    
                ],R => &[
                    
                ],Racket => &[
                    
                ],Rakefile => &[
                    
                ],Raku => &[
                    ("#|{","}",),("#={","}",),("#|(",")",),("#=(",")",),("#|[","]",),("#=[","]",),("#|｢","｣",),("#=｢","｣",),("=begin pod","=end pod",),("=begin code","=end code",),("=begin head","=end head",),("=begin item","=end item",),("=begin table","=end table",),("=begin defn","=end defn",),("=begin para","=end para",),("=begin comment","=end comment",),("=begin data","=end data",),("=begin DESCRIPTION","=end DESCRIPTION",),("=begin SYNOPSIS","=end SYNOPSIS",),("=begin ","=end ",),
                ],Razor => &[
                    
                ],Redscript => &[
                    
                ],Renpy => &[
                    
                ],ReScript => &[
                    
                ],ReStructuredText => &[
                    
                ],Roc => &[
                    ("\"\"\"","\"\"\"",),
                ],RON => &[
                    
                ],RPMSpecfile => &[
                    
                ],Ruby => &[
                    
                ],RubyHtml => &[
                    
                ],Rust => &[
                    
                ],Sass => &[
                    
                ],Scala => &[
                    
                ],Scheme => &[
                    
                ],Scons => &[
                    
                ],Sh => &[
                    
                ],ShaderLab => &[
                    
                ],SIL => &[
                    
                ],Slang => &[
                    
                ],Sml => &[
                    
                ],Smalltalk => &[
                    
                ],Snakemake => &[
                    ("\"\"\"","\"\"\"",),("'''","'''",),
                ],Solidity => &[
                    
                ],SpecmanE => &[
                    
                ],Spice => &[
                    
                ],Sql => &[
                    
                ],Sqf => &[
                    
                ],SRecode => &[
                    
                ],Stan => &[
                    
                ],Stata => &[
                    
                ],Stratego => &[
                    
                ],Stylus => &[
                    
                ],Svelte => &[
                    
                ],Svg => &[
                    
                ],Swift => &[
                    
                ],Swig => &[
                    
                ],SystemVerilog => &[
                    
                ],Slint => &[
                    
                ],Tact => &[
                    
                ],Tcl => &[
                    
                ],Tera => &[
                    
                ],Templ => &[
                    
                ],Tex => &[
                    
                ],Text => &[
                    
                ],Thrift => &[
                    
                ],Toml => &[
                    
                ],Tsx => &[
                    
                ],Ttcn => &[
                    
                ],Twig => &[
                    
                ],TypeScript => &[
                    
                ],Typst => &[
                    
                ],Uiua => &[
                    
                ],UMPL => &[
                    
                ],Unison => &[
                    
                ],UnrealDeveloperMarkdown => &[
                    
                ],UnrealPlugin => &[
                    
                ],UnrealProject => &[
                    
                ],UnrealScript => &[
                    
                ],UnrealShader => &[
                    
                ],UnrealShaderHeader => &[
                    
                ],UrWeb => &[
                    
                ],UrWebProject => &[
                    
                ],Vala => &[
                    
                ],VB6 => &[
                    
                ],VBScript => &[
                    
                ],Velocity => &[
                    
                ],Verilog => &[
                    
                ],VerilogArgsFile => &[
                    
                ],Vhdl => &[
                    
                ],Virgil => &[
                    
                ],VisualBasic => &[
                    
                ],VisualStudioProject => &[
                    
                ],VisualStudioSolution => &[
                    
                ],VimScript => &[
                    
                ],Vue => &[
                    
                ],WebAssembly => &[
                    
                ],WenYan => &[
                    
                ],WGSL => &[
                    
                ],Wolfram => &[
                    
                ],Xaml => &[
                    
                ],XcodeConfig => &[
                    
                ],Xml => &[
                    
                ],XSL => &[
                    
                ],Xtend => &[
                    
                ],Yaml => &[
                    
                ],ZenCode => &[
                    
                ],Zig => &[
                    
                ],Zokrates => &[
                    
                ],Zsh => &[
                    
                ],GdShader => &[
                    
                ],
        }
    }

    /// Returns the shebang of a language.
    /// ```
    /// use tokei::LanguageType;
    /// let lang = LanguageType::Bash;
    /// assert_eq!(lang.shebangs(), &["#!/bin/bash"]);
    /// ```
    pub fn shebangs(self) -> &'static [&'static str] {
        match self {
            Abap => &[],
            ABNF => &[],
            ActionScript => &[],
            Ada => &[],
            Agda => &[],
            Alex => &[],
            Alloy => &[],
            Apl => &[],
            Arduino => &[],
            ArkTS => &[],
            Arturo => &[],
            AsciiDoc => &[],
            Asn1 => &[],
            Asp => &[],
            AspNet => &[],
            Assembly => &[],
            AssemblyGAS => &[],
            Astro => &[],
            Ats => &[],
            Autoconf => &[],
            Autoit => &[],
            AutoHotKey => &[],
            Automake => &[],
            AvaloniaXaml => &[],
            AWK => &["#!/bin/awk -f",],
            Ballerina => &[],
            Bash => &["#!/bin/bash",],
            Batch => &[],
            Bazel => &[],
            Bean => &[],
            Bicep => &[],
            Bitbake => &[],
            Bqn => &[],
            BrightScript => &[],
            C => &[],
            Cabal => &[],
            Cairo => &[],
            Cangjie => &[],
            Cassius => &[],
            Ceylon => &[],
            Chapel => &[],
            CHeader => &[],
            Cil => &[],
            Circom => &[],
            Clojure => &[],
            ClojureC => &[],
            ClojureScript => &[],
            CMake => &[],
            Cobol => &[],
            CodeQL => &[],
            CoffeeScript => &[],
            Cogent => &[],
            ColdFusion => &[],
            ColdFusionScript => &[],
            Coq => &[],
            Cpp => &[],
            CppHeader => &[],
            CppModule => &[],
            Crystal => &["#!/usr/bin/crystal",],
            CSharp => &[],
            CShell => &["#!/bin/csh",],
            Css => &[],
            Cuda => &[],
            Cue => &[],
            Cython => &[],
            D => &[],
            D2 => &[],
            Daml => &[],
            Dart => &[],
            DeviceTree => &[],
            Dhall => &[],
            Dockerfile => &[],
            DotNetResource => &[],
            DreamMaker => &[],
            Dust => &[],
            Ebuild => &[],
            EdgeQL => &[],
            ESDL => &[],
            Edn => &[],
            Eighth => &[],
            Elisp => &[],
            Elixir => &[],
            Elm => &[],
            Elvish => &[],
            EmacsDevEnv => &[],
            Emojicode => &[],
            Erlang => &[],
            Factor => &[],
            FEN => &[],
            Fennel => &[],
            Fish => &["#!/bin/fish",],
            FlatBuffers => &[],
            ForgeConfig => &[],
            Forth => &[],
            FortranLegacy => &[],
            FortranModern => &[],
            FreeMarker => &[],
            FSharp => &[],
            Fstar => &[],
            Futhark => &[],
            GDB => &[],
            GdScript => &[],
            Gherkin => &[],
            Gleam => &[],
            GlimmerJs => &[],
            GlimmerTs => &[],
            Glsl => &[],
            Gml => &[],
            Go => &[],
            Gohtml => &[],
            Graphql => &[],
            Groovy => &[],
            Gwion => &[],
            Haml => &[],
            Hamlet => &[],
            Happy => &[],
            Handlebars => &[],
            Haskell => &[],
            Haxe => &[],
            Hcl => &[],
            Headache => &[],
            Hex => &[],
            Hex0 => &[],
            Hex1 => &[],
            Hex2 => &[],
            HiCad => &[],
            Hlsl => &[],
            HolyC => &[],
            Html => &[],
            Hy => &[],
            Idris => &[],
            Ini => &[],
            IntelHex => &[],
            Isabelle => &[],
            Jai => &[],
            Janet => &[],
            Java => &[],
            JavaScript => &[],
            Jinja2 => &[],
            Jq => &[],
            JSLT => &[],
            Json => &[],
            Jsonnet => &[],
            Jsx => &[],
            Julia => &[],
            Julius => &[],
            Jupyter => &[],
            Just => &["#!/usr/bin/env just --justfile",],
            K => &[],
            KakouneScript => &[],
            Kaem => &[],
            Koka => &[],
            Kotlin => &[],
            Ksh => &["#!/bin/ksh",],
            Lalrpop => &[],
            KvLanguage => &[],
            Lean => &[],
            Hledger => &[],
            Less => &[],
            Lex => &[],
            Liquid => &[],
            LinguaFranca => &[],
            LinkerScript => &[],
            Lisp => &[],
            LiveScript => &[],
            LLVM => &[],
            Logtalk => &[],
            LolCode => &[],
            Lua => &[],
            Lucius => &[],
            M1Assembly => &[],
            M4 => &[],
            Madlang => &[],
            Makefile => &[],
            Markdown => &[],
            Max => &[],
            Mdx => &[],
            Menhir => &[],
            Meson => &[],
            Metal => &[],
            Mint => &[],
            Mlatu => &[],
            Modelica => &[],
            ModuleDef => &[],
            Mojo => &[],
            MonkeyC => &[],
            MoonBit => &[],
            MoonScript => &[],
            MsBuild => &[],
            Mustache => &[],
            Nextflow => &[],
            Nim => &[],
            Nix => &[],
            NotQuitePerl => &[],
            NuGetConfig => &[],
            Nushell => &[],
            ObjectiveC => &[],
            ObjectiveCpp => &[],
            OCaml => &[],
            Odin => &[],
            OpenScad => &[],
            OpenPolicyAgent => &[],
            OpenCL => &[],
            OpenQasm => &[],
            OpenType => &[],
            Org => &[],
            Oz => &[],
            PacmanMakepkg => &[],
            Pan => &[],
            Pascal => &[],
            Perl => &["#!/usr/bin/perl",],
            Pest => &[],
            Phix => &[],
            Php => &[],
            PlantUml => &[],
            Po => &[],
            Poke => &[],
            Polly => &[],
            Pony => &[],
            PostCss => &[],
            PowerShell => &[],
            PRACTICE => &[],
            Processing => &[],
            Prolog => &[],
            PSL => &[],
            Protobuf => &[],
            Pug => &[],
            Puppet => &[],
            PureScript => &[],
            Pyret => &[],
            Python => &[],
            PRQL => &[],
            Q => &[],
            Qcl => &[],
            Qml => &[],
            R => &[],
            Racket => &[],
            Rakefile => &[],
            Raku => &["#!/usr/bin/raku","#!/usr/bin/perl6",],
            Razor => &[],
            Redscript => &[],
            Renpy => &[],
            ReScript => &[],
            ReStructuredText => &[],
            Roc => &[],
            RON => &[],
            RPMSpecfile => &[],
            Ruby => &[],
            RubyHtml => &[],
            Rust => &[],
            Sass => &[],
            Scala => &[],
            Scheme => &[],
            Scons => &[],
            Sh => &["#!/bin/sh",],
            ShaderLab => &[],
            SIL => &[],
            Slang => &[],
            Sml => &[],
            Smalltalk => &[],
            Snakemake => &[],
            Solidity => &[],
            SpecmanE => &[],
            Spice => &[],
            Sql => &[],
            Sqf => &[],
            SRecode => &[],
            Stan => &[],
            Stata => &[],
            Stratego => &[],
            Stylus => &[],
            Svelte => &[],
            Svg => &[],
            Swift => &[],
            Swig => &[],
            SystemVerilog => &[],
            Slint => &[],
            Tact => &[],
            Tcl => &[],
            Tera => &[],
            Templ => &[],
            Tex => &[],
            Text => &[],
            Thrift => &[],
            Toml => &[],
            Tsx => &[],
            Ttcn => &[],
            Twig => &[],
            TypeScript => &[],
            Typst => &[],
            Uiua => &[],
            UMPL => &[],
            Unison => &[],
            UnrealDeveloperMarkdown => &[],
            UnrealPlugin => &[],
            UnrealProject => &[],
            UnrealScript => &[],
            UnrealShader => &[],
            UnrealShaderHeader => &[],
            UrWeb => &[],
            UrWebProject => &[],
            Vala => &[],
            VB6 => &[],
            VBScript => &[],
            Velocity => &[],
            Verilog => &[],
            VerilogArgsFile => &[],
            Vhdl => &[],
            Virgil => &[],
            VisualBasic => &[],
            VisualStudioProject => &[],
            VisualStudioSolution => &[],
            VimScript => &[],
            Vue => &[],
            WebAssembly => &[],
            WenYan => &[],
            WGSL => &[],
            Wolfram => &[],
            Xaml => &[],
            XcodeConfig => &[],
            Xml => &[],
            XSL => &[],
            Xtend => &[],
            Yaml => &[],
            ZenCode => &[],
            Zig => &[],
            Zokrates => &[],
            Zsh => &["#!/bin/zsh",],
            GdShader => &[],
            
        }
    }

    pub(crate) fn any_multi_line_comments(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Abap => &[],
            ABNF => &[],
            ActionScript => &[("/*", "*/"),],
            Ada => &[],
            Agda => &[("{-", "-}"),],
            Alex => &[],
            Alloy => &[("/*", "*/"),],
            Apl => &[],
            Arduino => &[("/*", "*/"),],
            ArkTS => &[("/*", "*/"),],
            Arturo => &[],
            AsciiDoc => &[("////", "////"),],
            Asn1 => &[("/*", "*/"),],
            Asp => &[],
            AspNet => &[("<!--", "-->"),("<%--", "-->"),],
            Assembly => &[],
            AssemblyGAS => &[("/*", "*/"),],
            Astro => &[("/*", "*/"),("<!--", "-->"),],
            Ats => &[("(*", "*)"),("/*", "*/"),],
            Autoconf => &[],
            Autoit => &[("#comments-start", "#comments-end"),("#cs", "#ce"),],
            AutoHotKey => &[("/*", "*/"),],
            Automake => &[],
            AvaloniaXaml => &[("<!--", "-->"),],
            AWK => &[],
            Ballerina => &[],
            Bash => &[],
            Batch => &[],
            Bazel => &[],
            Bean => &[],
            Bicep => &[("/*", "*/"),],
            Bitbake => &[],
            Bqn => &[],
            BrightScript => &[],
            C => &[("/*", "*/"),],
            Cabal => &[("{-", "-}"),],
            Cairo => &[],
            Cangjie => &[("/*", "*/"),],
            Cassius => &[("/*", "*/"),],
            Ceylon => &[("/*", "*/"),],
            Chapel => &[("/*", "*/"),],
            CHeader => &[("/*", "*/"),],
            Cil => &[],
            Circom => &[("/*", "*/"),],
            Clojure => &[],
            ClojureC => &[],
            ClojureScript => &[],
            CMake => &[],
            Cobol => &[],
            CodeQL => &[("/*", "*/"),],
            CoffeeScript => &[("###", "###"),],
            Cogent => &[],
            ColdFusion => &[("<!---", "--->"),],
            ColdFusionScript => &[("/*", "*/"),],
            Coq => &[("(*", "*)"),],
            Cpp => &[("/*", "*/"),],
            CppHeader => &[("/*", "*/"),],
            CppModule => &[("/*", "*/"),],
            Crystal => &[],
            CSharp => &[("/*", "*/"),],
            CShell => &[],
            Css => &[("/*", "*/"),],
            Cuda => &[("/*", "*/"),],
            Cue => &[],
            Cython => &[],
            D => &[("/*", "*/"),("/+", "+/"),],
            D2 => &[("\"\"\"", "\"\"\""),],
            Daml => &[("{-", "-}"),],
            Dart => &[("/*", "*/"),],
            DeviceTree => &[("/*", "*/"),],
            Dhall => &[("{-", "-}"),],
            Dockerfile => &[],
            DotNetResource => &[("<!--", "-->"),],
            DreamMaker => &[("/*", "*/"),],
            Dust => &[("{!", "!}"),],
            Ebuild => &[],
            EdgeQL => &[],
            ESDL => &[],
            Edn => &[],
            Eighth => &[("(*", "*)"),],
            Elisp => &[],
            Elixir => &[],
            Elm => &[("{-", "-}"),],
            Elvish => &[],
            EmacsDevEnv => &[],
            Emojicode => &[("💭🔜", "🔚💭"),("📗", "📗"),("📘", "📘"),],
            Erlang => &[],
            Factor => &[("/*", "*/"),],
            FEN => &[],
            Fennel => &[],
            Fish => &[],
            FlatBuffers => &[("/*", "*/"),],
            ForgeConfig => &[],
            Forth => &[("( ", ")"),],
            FortranLegacy => &[],
            FortranModern => &[],
            FreeMarker => &[("<#--", "-->"),],
            FSharp => &[("(*", "*)"),],
            Fstar => &[("(*", "*)"),],
            Futhark => &[],
            GDB => &[],
            GdScript => &[],
            Gherkin => &[],
            Gleam => &[],
            GlimmerJs => &[("/*", "*/"),("<!--", "-->"),],
            GlimmerTs => &[("/*", "*/"),("<!--", "-->"),],
            Glsl => &[("/*", "*/"),],
            Gml => &[("/*", "*/"),],
            Go => &[("/*", "*/"),],
            Gohtml => &[("<!--", "-->"),("{{/*", "*/}}"),],
            Graphql => &[],
            Groovy => &[("/*", "*/"),],
            Gwion => &[],
            Haml => &[],
            Hamlet => &[("<!--", "-->"),],
            Happy => &[],
            Handlebars => &[("<!--", "-->"),("{{!", "}}"),],
            Haskell => &[("{-", "-}"),],
            Haxe => &[("/*", "*/"),],
            Hcl => &[("/*", "*/"),],
            Headache => &[("/*", "*/"),],
            Hex => &[],
            Hex0 => &[],
            Hex1 => &[],
            Hex2 => &[],
            HiCad => &[],
            Hlsl => &[("/*", "*/"),],
            HolyC => &[("/*", "*/"),],
            Html => &[("<!--", "-->"),],
            Hy => &[],
            Idris => &[("{-", "-}"),],
            Ini => &[],
            IntelHex => &[],
            Isabelle => &[("{*", "*}"),("(*", "*)"),("‹", "›"),("\\<open>", "\\<close>"),],
            Jai => &[("/*", "*/"),],
            Janet => &[],
            Java => &[("/*", "*/"),],
            JavaScript => &[("/*", "*/"),],
            Jinja2 => &[("{#", "#}"),],
            Jq => &[],
            JSLT => &[],
            Json => &[],
            Jsonnet => &[("/*", "*/"),],
            Jsx => &[("/*", "*/"),],
            Julia => &[("#=", "=#"),],
            Julius => &[("/*", "*/"),],
            Jupyter => &[],
            Just => &[],
            K => &[],
            KakouneScript => &[],
            Kaem => &[],
            Koka => &[("/*", "*/"),],
            Kotlin => &[("/*", "*/"),],
            Ksh => &[],
            Lalrpop => &[],
            KvLanguage => &[],
            Lean => &[("/-", "-/"),],
            Hledger => &[("comment", "end comment"),],
            Less => &[("/*", "*/"),],
            Lex => &[("/*", "*/"),],
            Liquid => &[("<!--", "-->"),("{% comment %}", "{% endcomment %}"),],
            LinguaFranca => &[("/*", "*/"),],
            LinkerScript => &[("/*", "*/"),],
            Lisp => &[("#|", "|#"),],
            LiveScript => &[("/*", "*/"),],
            LLVM => &[],
            Logtalk => &[("/*", "*/"),],
            LolCode => &[("OBTW", "TLDR"),],
            Lua => &[("--[[", "]]"),],
            Lucius => &[("/*", "*/"),],
            M1Assembly => &[],
            M4 => &[],
            Madlang => &[("{#", "#}"),],
            Makefile => &[],
            Markdown => &[],
            Max => &[],
            Mdx => &[],
            Menhir => &[("(*", "*)"),("/*", "*/"),],
            Meson => &[],
            Metal => &[("/*", "*/"),],
            Mint => &[],
            Mlatu => &[],
            Modelica => &[("/*", "*/"),],
            ModuleDef => &[],
            Mojo => &[],
            MonkeyC => &[("/*", "*/"),],
            MoonBit => &[],
            MoonScript => &[],
            MsBuild => &[("<!--", "-->"),],
            Mustache => &[("{{!", "}}"),],
            Nextflow => &[("/*", "*/"),],
            Nim => &[],
            Nix => &[("/*", "*/"),],
            NotQuitePerl => &[("=begin", "=end"),],
            NuGetConfig => &[("<!--", "-->"),],
            Nushell => &[],
            ObjectiveC => &[("/*", "*/"),],
            ObjectiveCpp => &[("/*", "*/"),],
            OCaml => &[("(*", "*)"),],
            Odin => &[("/*", "*/"),],
            OpenScad => &[("/*", "*/"),],
            OpenPolicyAgent => &[],
            OpenCL => &[("/*", "*/"),],
            OpenQasm => &[("/*", "*/"),],
            OpenType => &[],
            Org => &[],
            Oz => &[("/*", "*/"),],
            PacmanMakepkg => &[],
            Pan => &[],
            Pascal => &[("{", "}"),("(*", "*)"),],
            Perl => &[("=pod", "=cut"),],
            Pest => &[],
            Phix => &[("/*", "*/"),("--/*", "--*/"),],
            Php => &[("/*", "*/"),],
            PlantUml => &[("/'", "'/"),],
            Po => &[],
            Poke => &[("/*", "*/"),],
            Polly => &[("<!--", "-->"),],
            Pony => &[("/*", "*/"),],
            PostCss => &[("/*", "*/"),],
            PowerShell => &[("<#", "#>"),],
            PRACTICE => &[],
            Processing => &[("/*", "*/"),],
            Prolog => &[("/*", "*/"),],
            PSL => &[("/*", "*/"),],
            Protobuf => &[],
            Pug => &[],
            Puppet => &[],
            PureScript => &[("{-", "-}"),],
            Pyret => &[("#|", "|#"),],
            Python => &[],
            PRQL => &[],
            Q => &[],
            Qcl => &[("/*", "*/"),],
            Qml => &[("/*", "*/"),],
            R => &[],
            Racket => &[("#|", "|#"),],
            Rakefile => &[("=begin", "=end"),],
            Raku => &[("#`(", ")"),("#`[", "]"),("#`{", "}"),("#`｢", "｣"),],
            Razor => &[("<!--", "-->"),("@*", "*@"),("/*", "*/"),],
            Redscript => &[("/*", "*/"),],
            Renpy => &[],
            ReScript => &[("/*", "*/"),],
            ReStructuredText => &[],
            Roc => &[],
            RON => &[("/*", "*/"),],
            RPMSpecfile => &[],
            Ruby => &[("=begin", "=end"),],
            RubyHtml => &[("<!--", "-->"),],
            Rust => &[("/*", "*/"),],
            Sass => &[("/*", "*/"),],
            Scala => &[("/*", "*/"),],
            Scheme => &[("#|", "|#"),],
            Scons => &[],
            Sh => &[],
            ShaderLab => &[("/*", "*/"),],
            SIL => &[("/*", "*/"),("/+", "+/"),],
            Slang => &[("/*", "*/"),],
            Sml => &[("(*", "*)"),],
            Smalltalk => &[("\"", "\""),],
            Snakemake => &[],
            Solidity => &[("/*", "*/"),],
            SpecmanE => &[("'>", "<'"),],
            Spice => &[],
            Sql => &[("/*", "*/"),],
            Sqf => &[("/*", "*/"),],
            SRecode => &[],
            Stan => &[("/*", "*/"),],
            Stata => &[("/*", "*/"),],
            Stratego => &[("/*", "*/"),],
            Stylus => &[("/*", "*/"),],
            Svelte => &[("<!--", "-->"),],
            Svg => &[("<!--", "-->"),],
            Swift => &[("/*", "*/"),],
            Swig => &[("/*", "*/"),],
            SystemVerilog => &[("/*", "*/"),],
            Slint => &[("/*", "*/"),],
            Tact => &[("/*", "*/"),],
            Tcl => &[],
            Tera => &[("<!--", "-->"),("{#", "#}"),],
            Templ => &[("<!--", "-->"),("/*", "*/"),],
            Tex => &[],
            Text => &[],
            Thrift => &[("/*", "*/"),],
            Toml => &[],
            Tsx => &[("/*", "*/"),],
            Ttcn => &[("/*", "*/"),],
            Twig => &[("<!--", "-->"),("{#", "#}"),],
            TypeScript => &[("/*", "*/"),],
            Typst => &[("/*", "*/"),],
            Uiua => &[],
            UMPL => &[],
            Unison => &[("{-", "-}"),],
            UnrealDeveloperMarkdown => &[],
            UnrealPlugin => &[],
            UnrealProject => &[],
            UnrealScript => &[("/*", "*/"),],
            UnrealShader => &[("/*", "*/"),],
            UnrealShaderHeader => &[("/*", "*/"),],
            UrWeb => &[("(*", "*)"),],
            UrWebProject => &[],
            Vala => &[("/*", "*/"),],
            VB6 => &[],
            VBScript => &[],
            Velocity => &[("#*", "*#"),],
            Verilog => &[("/*", "*/"),],
            VerilogArgsFile => &[],
            Vhdl => &[("/*", "*/"),],
            Virgil => &[("/*", "*/"),],
            VisualBasic => &[],
            VisualStudioProject => &[("<!--", "-->"),],
            VisualStudioSolution => &[],
            VimScript => &[],
            Vue => &[("<!--", "-->"),("/*", "*/"),],
            WebAssembly => &[],
            WenYan => &[("批曰。", "。"),("疏曰。", "。"),],
            WGSL => &[],
            Wolfram => &[("(*", "*)"),],
            Xaml => &[("<!--", "-->"),],
            XcodeConfig => &[],
            Xml => &[("<!--", "-->"),],
            XSL => &[("<!--", "-->"),],
            Xtend => &[("/*", "*/"),],
            Yaml => &[],
            ZenCode => &[("/*", "*/"),],
            Zig => &[],
            Zokrates => &[("/*", "*/"),],
            Zsh => &[],
            GdShader => &[("/*", "*/"),],
            
        }
    }

    pub(crate) fn any_comments(self) -> &'static [&'static str] {
        match self {
            Abap => &["*","\"",],
            ABNF => &[";",],
            ActionScript => &["/*",
                        "*/","//",],
            Ada => &["--",],
            Agda => &["{-",
                        "-}","--",],
            Alex => &[],
            Alloy => &["/*",
                        "*/","--","//",],
            Apl => &["⍝",],
            Arduino => &["/*",
                        "*/","//",],
            ArkTS => &["/*",
                        "*/","//",],
            Arturo => &[";",],
            AsciiDoc => &["////",
                        "////","//",],
            Asn1 => &["/*",
                        "*/","--",],
            Asp => &["'","REM",],
            AspNet => &["<!--",
                        "-->","<%--",
                        "-->",],
            Assembly => &[";",],
            AssemblyGAS => &["/*",
                        "*/","//",],
            Astro => &["/*",
                        "*/","<!--",
                        "-->","//",],
            Ats => &["(*",
                        "*)","/*",
                        "*/","//",],
            Autoconf => &["#","dnl",],
            Autoit => &["#comments-start",
                        "#comments-end","#cs",
                        "#ce",";",],
            AutoHotKey => &["/*",
                        "*/",";",],
            Automake => &["#",],
            AvaloniaXaml => &["<!--",
                        "-->",],
            AWK => &["#",],
            Ballerina => &["//","#",],
            Bash => &["#",],
            Batch => &["REM","::",],
            Bazel => &["#",],
            Bean => &[";",],
            Bicep => &["/*",
                        "*/","//",],
            Bitbake => &["#",],
            Bqn => &["#",],
            BrightScript => &["'","REM",],
            C => &["/*",
                        "*/","//",],
            Cabal => &["{-",
                        "-}","--",],
            Cairo => &["//",],
            Cangjie => &["/*",
                        "*/","//",],
            Cassius => &["/*",
                        "*/","//",],
            Ceylon => &["/*",
                        "*/","//",],
            Chapel => &["/*",
                        "*/","//",],
            CHeader => &["/*",
                        "*/","//",],
            Cil => &[";",],
            Circom => &["/*",
                        "*/","//",],
            Clojure => &[";",],
            ClojureC => &[";",],
            ClojureScript => &[";",],
            CMake => &["#",],
            Cobol => &["*",],
            CodeQL => &["/*",
                        "*/","//",],
            CoffeeScript => &["###",
                        "###","#",],
            Cogent => &["--",],
            ColdFusion => &["<!---",
                        "--->",],
            ColdFusionScript => &["/*",
                        "*/","//",],
            Coq => &["(*",
                        "*)",],
            Cpp => &["/*",
                        "*/","//",],
            CppHeader => &["/*",
                        "*/","//",],
            CppModule => &["/*",
                        "*/","//",],
            Crystal => &["#",],
            CSharp => &["/*",
                        "*/","//",],
            CShell => &["#",],
            Css => &["/*",
                        "*/","//",],
            Cuda => &["/*",
                        "*/","//",],
            Cue => &["//",],
            Cython => &["#",],
            D => &["/*",
                        "*/","/+",
                        "+/","//",],
            D2 => &["\"\"\"",
                        "\"\"\"","#",],
            Daml => &["{-",
                        "-}","-- ",],
            Dart => &["/*",
                        "*/","//",],
            DeviceTree => &["/*",
                        "*/","//",],
            Dhall => &["{-",
                        "-}","--",],
            Dockerfile => &["#",],
            DotNetResource => &["<!--",
                        "-->",],
            DreamMaker => &["/*",
                        "*/","//",],
            Dust => &["{!",
                        "!}",],
            Ebuild => &["#",],
            EdgeQL => &["#",],
            ESDL => &["#",],
            Edn => &[";",],
            Eighth => &["(*",
                        "*)","\\ ","-- ",],
            Elisp => &[";",],
            Elixir => &["#",],
            Elm => &["{-",
                        "-}","--",],
            Elvish => &["#",],
            EmacsDevEnv => &[";",],
            Emojicode => &["💭🔜",
                        "🔚💭","📗",
                        "📗","📘",
                        "📘","💭",],
            Erlang => &["%",],
            Factor => &["/*",
                        "*/","!","#!",],
            FEN => &[],
            Fennel => &[";",";;",],
            Fish => &["#",],
            FlatBuffers => &["/*",
                        "*/","//",],
            ForgeConfig => &["#","~",],
            Forth => &["( ",
                        ")","\\",],
            FortranLegacy => &["c","C","!","*",],
            FortranModern => &["!",],
            FreeMarker => &["<#--",
                        "-->",],
            FSharp => &["(*",
                        "*)","//",],
            Fstar => &["(*",
                        "*)","//",],
            Futhark => &["--",],
            GDB => &["#",],
            GdScript => &["#",],
            Gherkin => &["#",],
            Gleam => &["//","///","////",],
            GlimmerJs => &["/*",
                        "*/","<!--",
                        "-->","//",],
            GlimmerTs => &["/*",
                        "*/","<!--",
                        "-->","//",],
            Glsl => &["/*",
                        "*/","//",],
            Gml => &["/*",
                        "*/","//",],
            Go => &["/*",
                        "*/","//",],
            Gohtml => &["<!--",
                        "-->","{{/*",
                        "*/}}",],
            Graphql => &["#",],
            Groovy => &["/*",
                        "*/","//",],
            Gwion => &["#!",],
            Haml => &["-#",],
            Hamlet => &["<!--",
                        "-->",],
            Happy => &[],
            Handlebars => &["<!--",
                        "-->","{{!",
                        "}}",],
            Haskell => &["{-",
                        "-}","--",],
            Haxe => &["/*",
                        "*/","//",],
            Hcl => &["/*",
                        "*/","#","//",],
            Headache => &["/*",
                        "*/","//",],
            Hex => &[],
            Hex0 => &["#",";",],
            Hex1 => &["#",";",],
            Hex2 => &["#",";",],
            HiCad => &["REM","rem",],
            Hlsl => &["/*",
                        "*/","//",],
            HolyC => &["/*",
                        "*/","//",],
            Html => &["<!--",
                        "-->",],
            Hy => &[";",],
            Idris => &["{-",
                        "-}","--",],
            Ini => &[";","#",],
            IntelHex => &[],
            Isabelle => &["{*",
                        "*}","(*",
                        "*)","‹",
                        "›","\\<open>",
                        "\\<close>","--",],
            Jai => &["/*",
                        "*/","//",],
            Janet => &["#",],
            Java => &["/*",
                        "*/","//",],
            JavaScript => &["/*",
                        "*/","//",],
            Jinja2 => &["{#",
                        "#}",],
            Jq => &["#",],
            JSLT => &["//",],
            Json => &[],
            Jsonnet => &["/*",
                        "*/","//","#",],
            Jsx => &["/*",
                        "*/","//",],
            Julia => &["#=",
                        "=#","#",],
            Julius => &["/*",
                        "*/","//",],
            Jupyter => &[],
            Just => &["#",],
            K => &["/",],
            KakouneScript => &["#",],
            Kaem => &["#",],
            Koka => &["/*",
                        "*/","//",],
            Kotlin => &["/*",
                        "*/","//",],
            Ksh => &["#",],
            Lalrpop => &["//",],
            KvLanguage => &["# ",],
            Lean => &["/-",
                        "-/","--",],
            Hledger => &["comment",
                        "end comment",";","#",],
            Less => &["/*",
                        "*/","//",],
            Lex => &["/*",
                        "*/","//",],
            Liquid => &["<!--",
                        "-->","{% comment %}",
                        "{% endcomment %}",],
            LinguaFranca => &["/*",
                        "*/","//","#",],
            LinkerScript => &["/*",
                        "*/",],
            Lisp => &["#|",
                        "|#",";",],
            LiveScript => &["/*",
                        "*/","#",],
            LLVM => &[";",],
            Logtalk => &["/*",
                        "*/","%",],
            LolCode => &["OBTW",
                        "TLDR","BTW",],
            Lua => &["--[[",
                        "]]","--",],
            Lucius => &["/*",
                        "*/","//",],
            M1Assembly => &["#",";",],
            M4 => &["#","dnl",],
            Madlang => &["{#",
                        "#}","#",],
            Makefile => &["#",],
            Markdown => &[],
            Max => &[],
            Mdx => &[],
            Menhir => &["(*",
                        "*)","/*",
                        "*/","//",],
            Meson => &["#",],
            Metal => &["/*",
                        "*/","//",],
            Mint => &[],
            Mlatu => &["//",],
            Modelica => &["/*",
                        "*/","//",],
            ModuleDef => &[";",],
            Mojo => &["#",],
            MonkeyC => &["/*",
                        "*/","//",],
            MoonBit => &["//",],
            MoonScript => &["--",],
            MsBuild => &["<!--",
                        "-->",],
            Mustache => &["{{!",
                        "}}",],
            Nextflow => &["/*",
                        "*/","//",],
            Nim => &["#",],
            Nix => &["/*",
                        "*/","#",],
            NotQuitePerl => &["=begin",
                        "=end","#",],
            NuGetConfig => &["<!--",
                        "-->",],
            Nushell => &["#",],
            ObjectiveC => &["/*",
                        "*/","//",],
            ObjectiveCpp => &["/*",
                        "*/","//",],
            OCaml => &["(*",
                        "*)",],
            Odin => &["/*",
                        "*/","//",],
            OpenScad => &["/*",
                        "*/","//",],
            OpenPolicyAgent => &["#",],
            OpenCL => &["/*",
                        "*/",],
            OpenQasm => &["/*",
                        "*/","//",],
            OpenType => &["#",],
            Org => &["# ",],
            Oz => &["/*",
                        "*/","%",],
            PacmanMakepkg => &["#",],
            Pan => &["#",],
            Pascal => &["{",
                        "}","(*",
                        "*)","//",],
            Perl => &["=pod",
                        "=cut","#",],
            Pest => &["//",],
            Phix => &["/*",
                        "*/","--/*",
                        "--*/","--","//","#!",],
            Php => &["/*",
                        "*/","#","//",],
            PlantUml => &["/'",
                        "'/","'",],
            Po => &["#",],
            Poke => &["/*",
                        "*/",],
            Polly => &["<!--",
                        "-->",],
            Pony => &["/*",
                        "*/","//",],
            PostCss => &["/*",
                        "*/","//",],
            PowerShell => &["<#",
                        "#>","#",],
            PRACTICE => &[";","//",],
            Processing => &["/*",
                        "*/","//",],
            Prolog => &["/*",
                        "*/","%",],
            PSL => &["/*",
                        "*/","//",],
            Protobuf => &["//",],
            Pug => &["//","//-",],
            Puppet => &["#",],
            PureScript => &["{-",
                        "-}","--",],
            Pyret => &["#|",
                        "|#","#",],
            Python => &["#",],
            PRQL => &["#",],
            Q => &["/",],
            Qcl => &["/*",
                        "*/","//",],
            Qml => &["/*",
                        "*/","//",],
            R => &["#",],
            Racket => &["#|",
                        "|#",";",],
            Rakefile => &["=begin",
                        "=end","#",],
            Raku => &["#`(",
                        ")","#`[",
                        "]","#`{",
                        "}","#`｢",
                        "｣","#",],
            Razor => &["<!--",
                        "-->","@*",
                        "*@","/*",
                        "*/","//",],
            Redscript => &["/*",
                        "*/","//","///",],
            Renpy => &["#",],
            ReScript => &["/*",
                        "*/","//",],
            ReStructuredText => &[],
            Roc => &["#",],
            RON => &["/*",
                        "*/","//",],
            RPMSpecfile => &["#",],
            Ruby => &["=begin",
                        "=end","#",],
            RubyHtml => &["<!--",
                        "-->",],
            Rust => &["/*",
                        "*/","//",],
            Sass => &["/*",
                        "*/","//",],
            Scala => &["/*",
                        "*/","//",],
            Scheme => &["#|",
                        "|#",";",],
            Scons => &["#",],
            Sh => &["#",],
            ShaderLab => &["/*",
                        "*/","//",],
            SIL => &["/*",
                        "*/","/+",
                        "+/","//",],
            Slang => &["/*",
                        "*/","//",],
            Sml => &["(*",
                        "*)",],
            Smalltalk => &["\"",
                        "\"",],
            Snakemake => &["#",],
            Solidity => &["/*",
                        "*/","//",],
            SpecmanE => &["'>",
                        "<'","--","//",],
            Spice => &["*",],
            Sql => &["/*",
                        "*/","--",],
            Sqf => &["/*",
                        "*/","//",],
            SRecode => &[";;",],
            Stan => &["/*",
                        "*/","//","#",],
            Stata => &["/*",
                        "*/","//","*",],
            Stratego => &["/*",
                        "*/","//",],
            Stylus => &["/*",
                        "*/","//",],
            Svelte => &["<!--",
                        "-->",],
            Svg => &["<!--",
                        "-->",],
            Swift => &["/*",
                        "*/","//",],
            Swig => &["/*",
                        "*/","//",],
            SystemVerilog => &["/*",
                        "*/","//",],
            Slint => &["/*",
                        "*/","//",],
            Tact => &["/*",
                        "*/","//",],
            Tcl => &["#",],
            Tera => &["<!--",
                        "-->","{#",
                        "#}",],
            Templ => &["<!--",
                        "-->","/*",
                        "*/","//",],
            Tex => &["%",],
            Text => &[],
            Thrift => &["/*",
                        "*/","#","//",],
            Toml => &["#",],
            Tsx => &["/*",
                        "*/","//",],
            Ttcn => &["/*",
                        "*/","//",],
            Twig => &["<!--",
                        "-->","{#",
                        "#}",],
            TypeScript => &["/*",
                        "*/","//",],
            Typst => &["/*",
                        "*/","//",],
            Uiua => &["#",],
            UMPL => &["!",],
            Unison => &["{-",
                        "-}","--",],
            UnrealDeveloperMarkdown => &[],
            UnrealPlugin => &[],
            UnrealProject => &[],
            UnrealScript => &["/*",
                        "*/","//",],
            UnrealShader => &["/*",
                        "*/","//",],
            UnrealShaderHeader => &["/*",
                        "*/","//",],
            UrWeb => &["(*",
                        "*)",],
            UrWebProject => &["#",],
            Vala => &["/*",
                        "*/","//",],
            VB6 => &["'",],
            VBScript => &["'","REM",],
            Velocity => &["#*",
                        "*#","##",],
            Verilog => &["/*",
                        "*/","//",],
            VerilogArgsFile => &[],
            Vhdl => &["/*",
                        "*/","--",],
            Virgil => &["/*",
                        "*/","//",],
            VisualBasic => &["'",],
            VisualStudioProject => &["<!--",
                        "-->",],
            VisualStudioSolution => &[],
            VimScript => &["\"",],
            Vue => &["<!--",
                        "-->","/*",
                        "*/","//",],
            WebAssembly => &[";;",],
            WenYan => &["批曰。",
                        "。","疏曰。",
                        "。",],
            WGSL => &["//",],
            Wolfram => &["(*",
                        "*)",],
            Xaml => &["<!--",
                        "-->",],
            XcodeConfig => &["//",],
            Xml => &["<!--",
                        "-->",],
            XSL => &["<!--",
                        "-->",],
            Xtend => &["/*",
                        "*/","//",],
            Yaml => &["#",],
            ZenCode => &["/*",
                        "*/","//","#",],
            Zig => &["//",],
            Zokrates => &["/*",
                        "*/","//",],
            Zsh => &["#",],
            GdShader => &["/*",
                        "*/","//",],
            
        }
    }

    /// Returns the parts of syntax that determines whether tokei can skip large
    /// parts of analysis.
    pub fn important_syntax(self) -> &'static [&'static str] {
        match self {
            Abap => &[],
            ABNF => &[],
            ActionScript => &["\"","/*",],
            Ada => &[],
            Agda => &["{-",],
            Alex => &[],
            Alloy => &["/*",],
            Apl => &["'",],
            Arduino => &["\"","/*",],
            ArkTS => &["\"","'","`","/*",],
            Arturo => &["\"",],
            AsciiDoc => &["////",],
            Asn1 => &["\"","'","/*",],
            Asp => &[],
            AspNet => &["<!--","<%--",],
            Assembly => &["\"","'",],
            AssemblyGAS => &["\"","/*",],
            Astro => &["/*","<!--",],
            Ats => &["\"","(*","/*",],
            Autoconf => &[],
            Autoit => &["#comments-start","#cs",],
            AutoHotKey => &["/*",],
            Automake => &[],
            AvaloniaXaml => &["\"","'","<!--",],
            AWK => &[],
            Ballerina => &["\"","`",],
            Bash => &["\"","'",],
            Batch => &[],
            Bazel => &["\"","'","\"\"\"","'''",],
            Bean => &["\"",],
            Bicep => &["'''","'","/*",],
            Bitbake => &["\"","'",],
            Bqn => &["\"","'",],
            BrightScript => &["\"",],
            C => &["\"","/*",],
            Cabal => &["{-",],
            Cairo => &["\"","'",],
            Cangjie => &["\"\"\"","\"","/*",],
            Cassius => &["\"","'","/*",],
            Ceylon => &["\"\"\"","\"","/*",],
            Chapel => &["\"","'","/*",],
            CHeader => &["\"","/*",],
            Cil => &["\"",],
            Circom => &["/*",],
            Clojure => &["\"",],
            ClojureC => &["\"",],
            ClojureScript => &["\"",],
            CMake => &["\"",],
            Cobol => &[],
            CodeQL => &["\"","/*",],
            CoffeeScript => &["\"","'","###",],
            Cogent => &[],
            ColdFusion => &["\"","'","<!---",],
            ColdFusionScript => &["\"","/*",],
            Coq => &["\"","(*",],
            Cpp => &["\"","/*",],
            CppHeader => &["\"","/*",],
            CppModule => &["\"","/*",],
            Crystal => &["\"","'",],
            CSharp => &["\"","/*",],
            CShell => &[],
            Css => &["\"","'","/*",],
            Cuda => &["\"","/*",],
            Cue => &["\"\"\"","\"","'",],
            Cython => &["\"","'","\"\"\"","'''",],
            D => &["\"","'","/*","/+",],
            D2 => &["\"\"\"",],
            Daml => &["{-",],
            Dart => &["\"\"\"","'''","\"","'","/*",],
            DeviceTree => &["\"","/*",],
            Dhall => &["\"","''","{-",],
            Dockerfile => &["\"","'",],
            DotNetResource => &["\"","<!--",],
            DreamMaker => &["{\"","\"","'","/*",],
            Dust => &["{!",],
            Ebuild => &["\"","'",],
            EdgeQL => &["\"","'","$",],
            ESDL => &["\"","'",],
            Edn => &[],
            Eighth => &["\"","(*",],
            Elisp => &[],
            Elixir => &["\"\"\"","'''","\"","'",],
            Elm => &["{-",],
            Elvish => &["\"","'",],
            EmacsDevEnv => &[],
            Emojicode => &["❌🔤","💭🔜","📗","📘",],
            Erlang => &[],
            Factor => &["/*",],
            FEN => &[],
            Fennel => &["\"",],
            Fish => &["\"","'",],
            FlatBuffers => &["\"","/*",],
            ForgeConfig => &[],
            Forth => &["( ",],
            FortranLegacy => &["\"","'",],
            FortranModern => &["\"",],
            FreeMarker => &["<#--",],
            FSharp => &["\"","(*",],
            Fstar => &["\"","(*",],
            Futhark => &[],
            GDB => &[],
            GdScript => &["\"\"\"","\"","'",],
            Gherkin => &[],
            Gleam => &["\"",],
            GlimmerJs => &["\"","'","`","/*","<!--","<template","<style",],
            GlimmerTs => &["\"","'","`","/*","<!--","<template","<style",],
            Glsl => &["\"","/*",],
            Gml => &["\"","/*",],
            Go => &["\"","/*",],
            Gohtml => &["\"","'","<!--","{{/*",],
            Graphql => &["\"\"\"","\"",],
            Groovy => &["\"","/*",],
            Gwion => &["\"",],
            Haml => &["\"","'",],
            Hamlet => &["\"","'","<!--",],
            Happy => &[],
            Handlebars => &["\"","'","<!--","{{!",],
            Haskell => &["{-",],
            Haxe => &["\"","'","/*",],
            Hcl => &["\"","/*",],
            Headache => &["\"","/*",],
            Hex => &[],
            Hex0 => &[],
            Hex1 => &[],
            Hex2 => &[],
            HiCad => &[],
            Hlsl => &["\"","/*",],
            HolyC => &["\"","/*",],
            Html => &["\"","'","<!--","<script","<style",],
            Hy => &[],
            Idris => &["\"\"\"","\"","{-",],
            Ini => &[],
            IntelHex => &[],
            Isabelle => &["''","{*","(*","‹","\\<open>",],
            Jai => &["\"","/*",],
            Janet => &["\"","'","`",],
            Java => &["\"","/*",],
            JavaScript => &["\"","'","`","/*",],
            Jinja2 => &["{#",],
            Jq => &["\"",],
            JSLT => &["\"",],
            Json => &[],
            Jsonnet => &["\"","'","/*",],
            Jsx => &["\"","'","`","/*",],
            Julia => &["\"\"\"","\"","#=",],
            Julius => &["\"","'","`","/*",],
            Jupyter => &[],
            Just => &[],
            K => &["\"",],
            KakouneScript => &["\"","'",],
            Kaem => &[],
            Koka => &["\"","/*",],
            Kotlin => &["\"\"\"","\"","/*",],
            Ksh => &["\"","'",],
            Lalrpop => &["#\"","\"",],
            KvLanguage => &["\"","'","\"\"\"","'''",],
            Lean => &["/-",],
            Hledger => &["comment",],
            Less => &["\"","'","/*",],
            Lex => &["/*",],
            Liquid => &["\"","'","<!--","{% comment %}",],
            LinguaFranca => &["\"","/*","{=",],
            LinkerScript => &["\"","/*",],
            Lisp => &["#|",],
            LiveScript => &["\"","'","/*",],
            LLVM => &["\"","'",],
            Logtalk => &["\"","/*",],
            LolCode => &["\"","OBTW",],
            Lua => &["\"","'","--[[",],
            Lucius => &["\"","'","/*",],
            M1Assembly => &["\"",],
            M4 => &["`",],
            Madlang => &["{#",],
            Makefile => &[],
            Markdown => &["```",],
            Max => &[],
            Mdx => &["```",],
            Menhir => &["\"","(*","/*",],
            Meson => &["'''","'",],
            Metal => &["\"","/*",],
            Mint => &[],
            Mlatu => &["\"",],
            Modelica => &["\"","/*",],
            ModuleDef => &[],
            Mojo => &["\"","'","\"\"\"","'''",],
            MonkeyC => &["\"","/*",],
            MoonBit => &["\"",],
            MoonScript => &["\"","'",],
            MsBuild => &["\"","'","<!--",],
            Mustache => &["\"","'","{{!",],
            Nextflow => &["\"","/*",],
            Nim => &["\"\"\"","\"",],
            Nix => &["\"","/*",],
            NotQuitePerl => &["\"","'","=begin",],
            NuGetConfig => &["\"","'","<!--",],
            Nushell => &["\"","'",],
            ObjectiveC => &["\"","/*",],
            ObjectiveCpp => &["\"","/*",],
            OCaml => &["\"","(*",],
            Odin => &["\"","'","/*",],
            OpenScad => &["\"","'","/*",],
            OpenPolicyAgent => &["\"","`",],
            OpenCL => &["/*",],
            OpenQasm => &["/*",],
            OpenType => &[],
            Org => &[],
            Oz => &["\"","/*",],
            PacmanMakepkg => &["\"","'",],
            Pan => &["\"","'",],
            Pascal => &["'","{","(*",],
            Perl => &["\"","'","=pod",],
            Pest => &["\"","'",],
            Phix => &["\"","'","/*","--/*",],
            Php => &["\"","'","/*",],
            PlantUml => &["\"","/'",],
            Po => &[],
            Poke => &["/*",],
            Polly => &["\"","'","<!--",],
            Pony => &["\"","\"\"\"","/*",],
            PostCss => &["\"","'","/*",],
            PowerShell => &["\"@","\"","@'","'","<#",],
            PRACTICE => &["\"",],
            Processing => &["\"","/*",],
            Prolog => &["\"","/*",],
            PSL => &["\"","/*",],
            Protobuf => &[],
            Pug => &["#{\"","#{'","#{`",],
            Puppet => &["\"","'",],
            PureScript => &["{-",],
            Pyret => &["\"","'","#|",],
            Python => &["\"","'","\"\"\"","'''",],
            PRQL => &["\"","'",],
            Q => &["\"",],
            Qcl => &["\"","/*",],
            Qml => &["\"","'","/*",],
            R => &[],
            Racket => &["#|",],
            Rakefile => &["\"","'","=begin",],
            Raku => &["\"","'","#|{","#={","#|(","#=(","#|[","#=[","#|｢","#=｢","=begin pod","=begin code","=begin head","=begin item","=begin table","=begin defn","=begin para","=begin comment","=begin data","=begin DESCRIPTION","=begin SYNOPSIS","=begin ","#`(","#`[","#`{","#`｢",],
            Razor => &["\"","<!--","@*","/*",],
            Redscript => &["\"","/*",],
            Renpy => &["\"","'","`",],
            ReScript => &["\"","/*",],
            ReStructuredText => &[],
            Roc => &["\"","'","\"\"\"",],
            RON => &["\"","/*",],
            RPMSpecfile => &[],
            Ruby => &["\"","'","=begin",],
            RubyHtml => &["\"","'","<!--","<script","<style",],
            Rust => &["#\"","\"","/*","///","//!",],
            Sass => &["\"","'","/*",],
            Scala => &["\"","/*",],
            Scheme => &["#|",],
            Scons => &["\"\"\"","'''","\"","'",],
            Sh => &["\"","'",],
            ShaderLab => &["\"","/*",],
            SIL => &["\"","'","`","/*","/+",],
            Slang => &["\"","/*",],
            Sml => &["\"","(*",],
            Smalltalk => &["'","\"",],
            Snakemake => &["\"","'","\"\"\"","'''",],
            Solidity => &["\"","/*",],
            SpecmanE => &["'>",],
            Spice => &[],
            Sql => &["'","/*",],
            Sqf => &["\"","'","/*",],
            SRecode => &[],
            Stan => &["\"","/*",],
            Stata => &["/*",],
            Stratego => &["\"","$[","$<","${","/*",],
            Stylus => &["\"","'","/*",],
            Svelte => &["\"","'","<!--","<script","<style",],
            Svg => &["\"","'","<!--",],
            Swift => &["\"","/*",],
            Swig => &["\"","/*",],
            SystemVerilog => &["\"","/*",],
            Slint => &["\"","/*",],
            Tact => &["\"","/*",],
            Tcl => &["\"","'",],
            Tera => &["\"","'","<!--","{#",],
            Templ => &["\"","'","`","<!--","/*","templ","script","css",],
            Tex => &[],
            Text => &[],
            Thrift => &["\"","'","/*",],
            Toml => &["\"\"\"","'''","\"","'",],
            Tsx => &["\"","'","`","/*",],
            Ttcn => &["\"","/*",],
            Twig => &["\"","'","<!--","{#",],
            TypeScript => &["\"","'","`","/*",],
            Typst => &["\"","/*",],
            Uiua => &["\"",],
            UMPL => &["`",],
            Unison => &["\"","{-",],
            UnrealDeveloperMarkdown => &["```",],
            UnrealPlugin => &[],
            UnrealProject => &[],
            UnrealScript => &["\"","/*",],
            UnrealShader => &["\"","/*",],
            UnrealShaderHeader => &["\"","/*",],
            UrWeb => &["\"","(*",],
            UrWebProject => &[],
            Vala => &["\"","/*",],
            VB6 => &[],
            VBScript => &[],
            Velocity => &["\"","'","#*",],
            Verilog => &["\"","/*",],
            VerilogArgsFile => &[],
            Vhdl => &["/*",],
            Virgil => &["\"","/*",],
            VisualBasic => &["\"",],
            VisualStudioProject => &["\"","'","<!--",],
            VisualStudioSolution => &[],
            VimScript => &["\"","'",],
            Vue => &["\"","'","`","<!--","/*","<script","<style","<template",],
            WebAssembly => &["\"","'",],
            WenYan => &["批曰。","疏曰。",],
            WGSL => &[],
            Wolfram => &["\"","(*",],
            Xaml => &["\"","'","<!--",],
            XcodeConfig => &["\"","'",],
            Xml => &["\"","'","<!--",],
            XSL => &["\"","'","<!--",],
            Xtend => &["'''","\"","'","/*",],
            Yaml => &["\"","'",],
            ZenCode => &["\"","'","/*",],
            Zig => &["\"",],
            Zokrates => &["/*",],
            Zsh => &["\"","'",],
            GdShader => &["/*",],
            
        }
    }

    /// Get language from a file path. May open and read the file.
    ///
    /// ```no_run
    /// use tokei::{Config, LanguageType};
    ///
    /// let rust = LanguageType::from_path("./main.rs", &Config::default());
    ///
    /// assert_eq!(rust, Some(LanguageType::Rust));
    /// ```
    pub fn from_path<P: AsRef<Path>>(entry: P, _config: &Config)
        -> Option<Self>
    {
        let entry = entry.as_ref();

        if let Some(filename) = fsutils::get_filename(entry) {
            match &*filename {
                | "build"| "workspace"| "module"=> return Some(Bazel),
                    | "cmakelists.txt"=> return Some(CMake),
                    | "dockerfile"=> return Some(Dockerfile),
                    | "justfile"=> return Some(Just),
                    | "gnumakefile"| "makefile"=> return Some(Makefile),
                    | "meson.build"| "meson_options.txt"=> return Some(Meson),
                    | "nuget.config"| "packages.config"| "nugetdefaults.config"=> return Some(NuGetConfig),
                    | "pkgbuild"=> return Some(PacmanMakepkg),
                    | "rakefile"=> return Some(Rakefile),
                    | "sconstruct"| "sconscript"=> return Some(Scons),
                    | "snakefile"=> return Some(Snakemake),
                    
                _ => ()
            }
        }

        match fsutils::get_extension(entry) {
            Some(extension) => LanguageType::from_file_extension(extension.as_str()),
            None => LanguageType::from_shebang(entry),
        }
    }

    /// Get language from a file extension.
    ///
    /// ```no_run
    /// use tokei::LanguageType;
    ///
    /// let rust = LanguageType::from_file_extension("rs");
    ///
    /// assert_eq!(rust, Some(LanguageType::Rust));
    /// ```
    #[must_use]
    pub fn from_file_extension(extension: &str) -> Option<Self> {
        match extension {
            | "abap" => Some(Abap),
                | "abnf" => Some(ABNF),
                | "as" => Some(ActionScript),
                | "ada" | "adb" | "ads" | "pad" => Some(Ada),
                | "agda" => Some(Agda),
                | "x" => Some(Alex),
                | "als" => Some(Alloy),
                | "apl" | "aplf" | "apls" => Some(Apl),
                | "ino" => Some(Arduino),
                | "ets" => Some(ArkTS),
                | "art" => Some(Arturo),
                | "adoc" | "asciidoc" => Some(AsciiDoc),
                | "asn1" => Some(Asn1),
                | "asa" | "asp" => Some(Asp),
                | "asax" | "ascx" | "asmx" | "aspx" | "master" | "sitemap" | "webinfo" => Some(AspNet),
                | "asm" => Some(Assembly),
                | "s" => Some(AssemblyGAS),
                | "astro" => Some(Astro),
                | "dats" | "hats" | "sats" | "atxt" => Some(Ats),
                | "in" => Some(Autoconf),
                | "au3" => Some(Autoit),
                | "ahk" => Some(AutoHotKey),
                | "am" => Some(Automake),
                | "axaml" => Some(AvaloniaXaml),
                | "awk" => Some(AWK),
                | "bal" => Some(Ballerina),
                | "bash" => Some(Bash),
                | "bat" | "btm" | "cmd" => Some(Batch),
                | "bzl" | "bazel" | "bzlmod" => Some(Bazel),
                | "bean" | "beancount" => Some(Bean),
                | "bicep" | "bicepparam" => Some(Bicep),
                | "bb" | "bbclass" | "bbappend" | "inc" => Some(Bitbake),
                | "bqn" => Some(Bqn),
                | "brs" => Some(BrightScript),
                | "c" | "ec" | "pgc" => Some(C),
                | "cabal" => Some(Cabal),
                | "cairo" => Some(Cairo),
                | "cj" => Some(Cangjie),
                | "cassius" => Some(Cassius),
                | "ceylon" => Some(Ceylon),
                | "chpl" => Some(Chapel),
                | "h" => Some(CHeader),
                | "cil" => Some(Cil),
                | "circom" => Some(Circom),
                | "clj" => Some(Clojure),
                | "cljc" => Some(ClojureC),
                | "cljs" => Some(ClojureScript),
                | "cmake" => Some(CMake),
                | "cob" | "cbl" | "ccp" | "cobol" | "cpy" => Some(Cobol),
                | "ql" | "qll" => Some(CodeQL),
                | "coffee" | "cjsx" => Some(CoffeeScript),
                | "cogent" => Some(Cogent),
                | "cfm" => Some(ColdFusion),
                | "cfc" => Some(ColdFusionScript),
                | "v" => Some(Coq),
                | "cc" | "cpp" | "cxx" | "c++" | "pcc" | "tpp" => Some(Cpp),
                | "hh" | "hpp" | "hxx" | "inl" | "ipp" => Some(CppHeader),
                | "cppm" | "ixx" | "ccm" | "mpp" | "mxx" | "cxxm" | "hppm" | "hxxm" => Some(CppModule),
                | "cr" => Some(Crystal),
                | "cs" | "csx" => Some(CSharp),
                | "csh" => Some(CShell),
                | "css" => Some(Css),
                | "cu" => Some(Cuda),
                | "cue" => Some(Cue),
                | "pyx" | "pxd" | "pxi" => Some(Cython),
                | "d" => Some(D),
                | "d2" => Some(D2),
                | "daml" => Some(Daml),
                | "dart" => Some(Dart),
                | "dts" | "dtsi" => Some(DeviceTree),
                | "dhall" => Some(Dhall),
                | "dockerfile" | "dockerignore" => Some(Dockerfile),
                | "resx" => Some(DotNetResource),
                | "dm" | "dme" => Some(DreamMaker),
                | "dust" => Some(Dust),
                | "ebuild" | "eclass" => Some(Ebuild),
                | "edgeql" => Some(EdgeQL),
                | "esdl" => Some(ESDL),
                | "edn" => Some(Edn),
                | "8th" => Some(Eighth),
                | "el" => Some(Elisp),
                | "ex" | "exs" => Some(Elixir),
                | "elm" => Some(Elm),
                | "elv" => Some(Elvish),
                | "ede" => Some(EmacsDevEnv),
                | "emojic" | "🍇" => Some(Emojicode),
                | "erl" | "hrl" => Some(Erlang),
                | "factor" => Some(Factor),
                | "fen" => Some(FEN),
                | "fnl" | "fnlm" => Some(Fennel),
                | "fish" => Some(Fish),
                | "fbs" => Some(FlatBuffers),
                | "cfg" => Some(ForgeConfig),
                | "4th" | "forth" | "fr" | "frt" | "fth" | "f83" | "fb" | "fpm" | "e4" | "rx" | "ft" => Some(Forth),
                | "f" | "for" | "ftn" | "f77" | "pfo" => Some(FortranLegacy),
                | "f03" | "f08" | "f90" | "f95" | "fpp" => Some(FortranModern),
                | "ftl" | "ftlh" | "ftlx" => Some(FreeMarker),
                | "fs" | "fsi" | "fsx" | "fsscript" => Some(FSharp),
                | "fst" | "fsti" => Some(Fstar),
                | "fut" => Some(Futhark),
                | "gdb" => Some(GDB),
                | "gd" => Some(GdScript),
                | "feature" => Some(Gherkin),
                | "gleam" => Some(Gleam),
                | "gjs" => Some(GlimmerJs),
                | "gts" => Some(GlimmerTs),
                | "vert" | "tesc" | "tese" | "geom" | "frag" | "comp" | "mesh" | "task" | "rgen" | "rint" | "rahit" | "rchit" | "rmiss" | "rcall" | "glsl" => Some(Glsl),
                | "gml" => Some(Gml),
                | "go" => Some(Go),
                | "gohtml" => Some(Gohtml),
                | "gql" | "graphql" => Some(Graphql),
                | "groovy" | "grt" | "gtpl" | "gvy" => Some(Groovy),
                | "gw" => Some(Gwion),
                | "haml" => Some(Haml),
                | "hamlet" => Some(Hamlet),
                | "y" | "ly" => Some(Happy),
                | "hbs" | "handlebars" => Some(Handlebars),
                | "hs" => Some(Haskell),
                | "hx" => Some(Haxe),
                | "hcl" | "tf" | "tfvars" => Some(Hcl),
                | "ha" => Some(Headache),
                | "hex" => Some(Hex),
                | "hex0" => Some(Hex0),
                | "hex1" => Some(Hex1),
                | "hex2" => Some(Hex2),
                | "MAC" | "mac" => Some(HiCad),
                | "hlsl" | "fx" | "fxsub" => Some(Hlsl),
                | "HC" | "hc" | "ZC" | "zc" => Some(HolyC),
                | "html" | "htm" => Some(Html),
                | "hy" => Some(Hy),
                | "idr" | "lidr" => Some(Idris),
                | "ini" => Some(Ini),
                | "ihex" => Some(IntelHex),
                | "thy" => Some(Isabelle),
                | "jai" => Some(Jai),
                | "janet" => Some(Janet),
                | "java" => Some(Java),
                | "cjs" | "js" | "mjs" => Some(JavaScript),
                | "j2" | "jinja" => Some(Jinja2),
                | "jq" => Some(Jq),
                | "jslt" => Some(JSLT),
                | "json" => Some(Json),
                | "jsonnet" | "libsonnet" => Some(Jsonnet),
                | "jsx" => Some(Jsx),
                | "jl" => Some(Julia),
                | "julius" => Some(Julius),
                | "ipynb" => Some(Jupyter),
                | "just" => Some(Just),
                | "k" => Some(K),
                | "kak" => Some(KakouneScript),
                | "kaem" => Some(Kaem),
                | "kk" => Some(Koka),
                | "kt" | "kts" => Some(Kotlin),
                | "ksh" => Some(Ksh),
                | "lalrpop" => Some(Lalrpop),
                | "kv" => Some(KvLanguage),
                | "lean" | "hlean" => Some(Lean),
                | "hledger" => Some(Hledger),
                | "less" => Some(Less),
                | "l" | "lex" => Some(Lex),
                | "liquid" => Some(Liquid),
                | "lf" => Some(LinguaFranca),
                | "ld" | "lds" => Some(LinkerScript),
                | "lisp" | "lsp" | "asd" => Some(Lisp),
                | "ls" => Some(LiveScript),
                | "ll" => Some(LLVM),
                | "lgt" | "logtalk" => Some(Logtalk),
                | "lol" => Some(LolCode),
                | "lua" | "luau" => Some(Lua),
                | "lucius" => Some(Lucius),
                | "m1" => Some(M1Assembly),
                | "m4" => Some(M4),
                | "mad" => Some(Madlang),
                | "makefile" | "mak" | "mk" => Some(Makefile),
                | "md" | "markdown" => Some(Markdown),
                | "maxpat" => Some(Max),
                | "mdx" => Some(Mdx),
                | "mll" | "mly" | "vy" => Some(Menhir),
                | "metal" => Some(Metal),
                | "mint" => Some(Mint),
                | "mlt" => Some(Mlatu),
                | "mo" | "mos" => Some(Modelica),
                | "def" => Some(ModuleDef),
                | "mojo" | "🔥" => Some(Mojo),
                | "mc" => Some(MonkeyC),
                | "mbt" | "mbti" => Some(MoonBit),
                | "moon" => Some(MoonScript),
                | "csproj" | "vbproj" | "fsproj" | "props" | "targets" => Some(MsBuild),
                | "mustache" => Some(Mustache),
                | "nextflow" | "nf" => Some(Nextflow),
                | "nim" => Some(Nim),
                | "nix" => Some(Nix),
                | "nqp" => Some(NotQuitePerl),
                | "nu" => Some(Nushell),
                | "m" => Some(ObjectiveC),
                | "mm" => Some(ObjectiveCpp),
                | "ml" | "mli" | "re" | "rei" => Some(OCaml),
                | "odin" => Some(Odin),
                | "scad" => Some(OpenScad),
                | "rego" => Some(OpenPolicyAgent),
                | "cl" | "ocl" => Some(OpenCL),
                | "qasm" => Some(OpenQasm),
                | "fea" => Some(OpenType),
                | "org" => Some(Org),
                | "oz" => Some(Oz),
                | "pan" | "tpl" => Some(Pan),
                | "pas" => Some(Pascal),
                | "pl" | "pm" => Some(Perl),
                | "pest" => Some(Pest),
                | "e" | "exw" => Some(Phix),
                | "php" => Some(Php),
                | "puml" => Some(PlantUml),
                | "po" | "pot" => Some(Po),
                | "pk" => Some(Poke),
                | "polly" => Some(Polly),
                | "pony" => Some(Pony),
                | "pcss" | "sss" => Some(PostCss),
                | "ps1" | "psm1" | "psd1" | "ps1xml" | "cdxml" | "pssc" | "psc1" => Some(PowerShell),
                | "cmm" => Some(PRACTICE),
                | "pde" => Some(Processing),
                | "p" | "pro" => Some(Prolog),
                | "psl" => Some(PSL),
                | "proto" => Some(Protobuf),
                | "pug" => Some(Pug),
                | "pp" => Some(Puppet),
                | "purs" => Some(PureScript),
                | "arr" => Some(Pyret),
                | "py" | "pyw" | "pyi" => Some(Python),
                | "prql" => Some(PRQL),
                | "q" => Some(Q),
                | "qcl" => Some(Qcl),
                | "qml" => Some(Qml),
                | "r" => Some(R),
                | "rkt" | "scrbl" => Some(Racket),
                | "rake" => Some(Rakefile),
                | "raku" | "rakumod" | "rakutest" | "pm6" | "pl6" | "p6" => Some(Raku),
                | "cshtml" | "razor" => Some(Razor),
                | "reds" => Some(Redscript),
                | "rpy" => Some(Renpy),
                | "res" | "resi" => Some(ReScript),
                | "rst" => Some(ReStructuredText),
                | "roc" => Some(Roc),
                | "ron" => Some(RON),
                | "spec" => Some(RPMSpecfile),
                | "rb" => Some(Ruby),
                | "rhtml" | "erb" => Some(RubyHtml),
                | "rs" => Some(Rust),
                | "sass" | "scss" => Some(Sass),
                | "sc" | "scala" => Some(Scala),
                | "scm" | "ss" => Some(Scheme),
                | "sh" => Some(Sh),
                | "shader" | "cginc" => Some(ShaderLab),
                | "sil" => Some(SIL),
                | "slang" => Some(Slang),
                | "sml" => Some(Sml),
                | "cs.st" | "pck.st" => Some(Smalltalk),
                | "smk" | "rules" => Some(Snakemake),
                | "sol" => Some(Solidity),
                | "e" => Some(SpecmanE),
                | "ckt" => Some(Spice),
                | "sql" => Some(Sql),
                | "sqf" => Some(Sqf),
                | "srt" => Some(SRecode),
                | "stan" => Some(Stan),
                | "do" => Some(Stata),
                | "str" => Some(Stratego),
                | "styl" => Some(Stylus),
                | "svelte" => Some(Svelte),
                | "svg" => Some(Svg),
                | "swift" => Some(Swift),
                | "swg" | "i" => Some(Swig),
                | "sv" | "svh" => Some(SystemVerilog),
                | "slint" => Some(Slint),
                | "tact" => Some(Tact),
                | "tcl" => Some(Tcl),
                | "tera" => Some(Tera),
                | "templ" | "tmpl" => Some(Templ),
                | "tex" | "sty" => Some(Tex),
                | "text" | "txt" => Some(Text),
                | "thrift" => Some(Thrift),
                | "toml" => Some(Toml),
                | "tsx" => Some(Tsx),
                | "ttcn" | "ttcn3" | "ttcnpp" => Some(Ttcn),
                | "twig" => Some(Twig),
                | "ts" | "mts" | "cts" => Some(TypeScript),
                | "typ" => Some(Typst),
                | "ua" => Some(Uiua),
                | "umpl" => Some(UMPL),
                | "u" => Some(Unison),
                | "udn" => Some(UnrealDeveloperMarkdown),
                | "uplugin" => Some(UnrealPlugin),
                | "uproject" => Some(UnrealProject),
                | "uc" | "uci" | "upkg" => Some(UnrealScript),
                | "usf" => Some(UnrealShader),
                | "ush" => Some(UnrealShaderHeader),
                | "ur" | "urs" => Some(UrWeb),
                | "urp" => Some(UrWebProject),
                | "vala" => Some(Vala),
                | "frm" | "bas" | "cls" | "ctl" | "dsr" => Some(VB6),
                | "vbs" => Some(VBScript),
                | "vm" => Some(Velocity),
                | "vg" | "vh" => Some(Verilog),
                | "irunargs" | "xrunargs" => Some(VerilogArgsFile),
                | "vhd" | "vhdl" => Some(Vhdl),
                | "v3" => Some(Virgil),
                | "vb" => Some(VisualBasic),
                | "vcproj" | "vcxproj" => Some(VisualStudioProject),
                | "sln" => Some(VisualStudioSolution),
                | "vim" => Some(VimScript),
                | "vue" => Some(Vue),
                | "wat" | "wast" => Some(WebAssembly),
                | "wy" => Some(WenYan),
                | "wgsl" => Some(WGSL),
                | "nb" | "wl" => Some(Wolfram),
                | "xaml" => Some(Xaml),
                | "xcconfig" => Some(XcodeConfig),
                | "xml" => Some(Xml),
                | "xsl" | "xslt" => Some(XSL),
                | "xtend" => Some(Xtend),
                | "yaml" | "yml" => Some(Yaml),
                | "zs" => Some(ZenCode),
                | "zig" => Some(Zig),
                | "zok" => Some(Zokrates),
                | "zsh" => Some(Zsh),
                | "gdshader" => Some(GdShader),
                
            extension => {
                warn!("Unknown extension: {}", extension);
                None
            },
        }
    }

    /// Get language from its name.
    ///
    /// ```no_run
    /// use tokei::LanguageType;
    ///
    /// let rust = LanguageType::from_name("Rust");
    ///
    /// assert_eq!(rust, Some(LanguageType::Rust));
    /// ```
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            | "ABAP"
                | "Abap" => Some(Abap),
            | "ABNF" => Some(ABNF),
            | "ActionScript" => Some(ActionScript),
            | "Ada" => Some(Ada),
            | "Agda" => Some(Agda),
            | "Alex" => Some(Alex),
            | "Alloy" => Some(Alloy),
            | "APL"
                | "Apl" => Some(Apl),
            | "Arduino C++"
                | "Arduino" => Some(Arduino),
            | "Ark TypeScript"
                | "ArkTS" => Some(ArkTS),
            | "Arturo" => Some(Arturo),
            | "AsciiDoc" => Some(AsciiDoc),
            | "ASN.1"
                | "Asn1" => Some(Asn1),
            | "ASP"
                | "Asp" => Some(Asp),
            | "ASP.NET"
                | "AspNet" => Some(AspNet),
            | "Assembly" => Some(Assembly),
            | "GNU Style Assembly"
                | "AssemblyGAS" => Some(AssemblyGAS),
            | "Astro" => Some(Astro),
            | "ATS"
                | "Ats" => Some(Ats),
            | "Autoconf" => Some(Autoconf),
            | "Autoit" => Some(Autoit),
            | "AutoHotKey" => Some(AutoHotKey),
            | "Automake" => Some(Automake),
            | "AXAML"
                | "AvaloniaXaml" => Some(AvaloniaXaml),
            | "AWK" => Some(AWK),
            | "Ballerina" => Some(Ballerina),
            | "BASH"
                | "Bash" => Some(Bash),
            | "Batch" => Some(Batch),
            | "Bazel" => Some(Bazel),
            | "Bean" => Some(Bean),
            | "Bicep" => Some(Bicep),
            | "Bitbake" => Some(Bitbake),
            | "BQN"
                | "Bqn" => Some(Bqn),
            | "BrightScript" => Some(BrightScript),
            | "C" => Some(C),
            | "Cabal" => Some(Cabal),
            | "Cairo" => Some(Cairo),
            | "Cangjie" => Some(Cangjie),
            | "Cassius" => Some(Cassius),
            | "Ceylon" => Some(Ceylon),
            | "Chapel" => Some(Chapel),
            | "C Header"
                | "CHeader" => Some(CHeader),
            | "CIL (SELinux)"
                | "Cil" => Some(Cil),
            | "Circom" => Some(Circom),
            | "Clojure" => Some(Clojure),
            | "ClojureC" => Some(ClojureC),
            | "ClojureScript" => Some(ClojureScript),
            | "CMake" => Some(CMake),
            | "COBOL"
                | "Cobol" => Some(Cobol),
            | "CodeQL" => Some(CodeQL),
            | "CoffeeScript" => Some(CoffeeScript),
            | "Cogent" => Some(Cogent),
            | "ColdFusion" => Some(ColdFusion),
            | "ColdFusion CFScript"
                | "ColdFusionScript" => Some(ColdFusionScript),
            | "Coq" => Some(Coq),
            | "C++"
                | "Cpp" => Some(Cpp),
            | "C++ Header"
                | "CppHeader" => Some(CppHeader),
            | "C++ Module"
                | "CppModule" => Some(CppModule),
            | "Crystal" => Some(Crystal),
            | "C#"
                | "CSharp" => Some(CSharp),
            | "C Shell"
                | "CShell" => Some(CShell),
            | "CSS"
                | "Css" => Some(Css),
            | "CUDA"
                | "Cuda" => Some(Cuda),
            | "CUE"
                | "Cue" => Some(Cue),
            | "Cython" => Some(Cython),
            | "D" => Some(D),
            | "D2" => Some(D2),
            | "DAML"
                | "Daml" => Some(Daml),
            | "Dart" => Some(Dart),
            | "Device Tree"
                | "DeviceTree" => Some(DeviceTree),
            | "Dhall" => Some(Dhall),
            | "Dockerfile" => Some(Dockerfile),
            | ".NET Resource"
                | "DotNetResource" => Some(DotNetResource),
            | "Dream Maker"
                | "DreamMaker" => Some(DreamMaker),
            | "Dust.js"
                | "Dust" => Some(Dust),
            | "Ebuild" => Some(Ebuild),
            | "EdgeQL" => Some(EdgeQL),
            | "EdgeDB Schema Definition"
                | "ESDL" => Some(ESDL),
            | "Edn" => Some(Edn),
            | "8th"
                | "Eighth" => Some(Eighth),
            | "Emacs Lisp"
                | "Elisp" => Some(Elisp),
            | "Elixir" => Some(Elixir),
            | "Elm" => Some(Elm),
            | "Elvish" => Some(Elvish),
            | "Emacs Dev Env"
                | "EmacsDevEnv" => Some(EmacsDevEnv),
            | "Emojicode" => Some(Emojicode),
            | "Erlang" => Some(Erlang),
            | "Factor" => Some(Factor),
            | "FEN" => Some(FEN),
            | "Fennel" => Some(Fennel),
            | "Fish" => Some(Fish),
            | "FlatBuffers Schema"
                | "FlatBuffers" => Some(FlatBuffers),
            | "Forge Config"
                | "ForgeConfig" => Some(ForgeConfig),
            | "Forth" => Some(Forth),
            | "FORTRAN Legacy"
                | "FortranLegacy" => Some(FortranLegacy),
            | "FORTRAN Modern"
                | "FortranModern" => Some(FortranModern),
            | "FreeMarker" => Some(FreeMarker),
            | "F#"
                | "FSharp" => Some(FSharp),
            | "F*"
                | "Fstar" => Some(Fstar),
            | "Futhark" => Some(Futhark),
            | "GDB Script"
                | "GDB" => Some(GDB),
            | "GDScript"
                | "GdScript" => Some(GdScript),
            | "Gherkin (Cucumber)"
                | "Gherkin" => Some(Gherkin),
            | "Gleam" => Some(Gleam),
            | "Glimmer JS"
                | "GlimmerJs" => Some(GlimmerJs),
            | "Glimmer TS"
                | "GlimmerTs" => Some(GlimmerTs),
            | "GLSL"
                | "Glsl" => Some(Glsl),
            | "Gml" => Some(Gml),
            | "Go" => Some(Go),
            | "Go HTML"
                | "Gohtml" => Some(Gohtml),
            | "GraphQL"
                | "Graphql" => Some(Graphql),
            | "Groovy" => Some(Groovy),
            | "Gwion" => Some(Gwion),
            | "Haml" => Some(Haml),
            | "Hamlet" => Some(Hamlet),
            | "Happy" => Some(Happy),
            | "Handlebars" => Some(Handlebars),
            | "Haskell" => Some(Haskell),
            | "Haxe" => Some(Haxe),
            | "HCL"
                | "Hcl" => Some(Hcl),
            | "Headache" => Some(Headache),
            | "HEX"
                | "Hex" => Some(Hex),
            | "Hex0" => Some(Hex0),
            | "Hex1" => Some(Hex1),
            | "Hex2" => Some(Hex2),
            | "HICAD"
                | "HiCad" => Some(HiCad),
            | "HLSL"
                | "Hlsl" => Some(Hlsl),
            | "HolyC" => Some(HolyC),
            | "HTML"
                | "Html" => Some(Html),
            | "Hy" => Some(Hy),
            | "Idris" => Some(Idris),
            | "INI"
                | "Ini" => Some(Ini),
            | "Intel HEX"
                | "IntelHex" => Some(IntelHex),
            | "Isabelle" => Some(Isabelle),
            | "JAI"
                | "Jai" => Some(Jai),
            | "Janet" => Some(Janet),
            | "Java" => Some(Java),
            | "JavaScript" => Some(JavaScript),
            | "Jinja2" => Some(Jinja2),
            | "jq"
                | "Jq" => Some(Jq),
            | "JSLT" => Some(JSLT),
            | "JSON"
                | "Json" => Some(Json),
            | "Jsonnet" => Some(Jsonnet),
            | "JSX"
                | "Jsx" => Some(Jsx),
            | "Julia" => Some(Julia),
            | "Julius" => Some(Julius),
            | "Jupyter Notebooks"
                | "Jupyter" => Some(Jupyter),
            | "Just" => Some(Just),
            | "K" => Some(K),
            | "Kakoune script"
                | "KakouneScript" => Some(KakouneScript),
            | "Kaem" => Some(Kaem),
            | "Koka" => Some(Koka),
            | "Kotlin" => Some(Kotlin),
            | "Korn shell"
                | "Ksh" => Some(Ksh),
            | "LALRPOP"
                | "Lalrpop" => Some(Lalrpop),
            | "KV Language"
                | "KvLanguage" => Some(KvLanguage),
            | "Lean" => Some(Lean),
            | "hledger"
                | "Hledger" => Some(Hledger),
            | "LESS"
                | "Less" => Some(Less),
            | "Lex" => Some(Lex),
            | "Liquid" => Some(Liquid),
            | "Lingua Franca"
                | "LinguaFranca" => Some(LinguaFranca),
            | "LD Script"
                | "LinkerScript" => Some(LinkerScript),
            | "Common Lisp"
                | "Lisp" => Some(Lisp),
            | "LiveScript" => Some(LiveScript),
            | "LLVM" => Some(LLVM),
            | "Logtalk" => Some(Logtalk),
            | "LOLCODE"
                | "LolCode" => Some(LolCode),
            | "Lua" => Some(Lua),
            | "Lucius" => Some(Lucius),
            | "M1 Assembly"
                | "M1Assembly" => Some(M1Assembly),
            | "M4" => Some(M4),
            | "Madlang" => Some(Madlang),
            | "Makefile" => Some(Makefile),
            | "Markdown" => Some(Markdown),
            | "Max" => Some(Max),
            | "MDX"
                | "Mdx" => Some(Mdx),
            | "Menhir" => Some(Menhir),
            | "Meson" => Some(Meson),
            | "Metal Shading Language"
                | "Metal" => Some(Metal),
            | "Mint" => Some(Mint),
            | "Mlatu" => Some(Mlatu),
            | "Modelica" => Some(Modelica),
            | "Module-Definition"
                | "ModuleDef" => Some(ModuleDef),
            | "Mojo" => Some(Mojo),
            | "Monkey C"
                | "MonkeyC" => Some(MonkeyC),
            | "MoonBit" => Some(MoonBit),
            | "MoonScript" => Some(MoonScript),
            | "MSBuild"
                | "MsBuild" => Some(MsBuild),
            | "Mustache" => Some(Mustache),
            | "Nextflow" => Some(Nextflow),
            | "Nim" => Some(Nim),
            | "Nix" => Some(Nix),
            | "Not Quite Perl"
                | "NotQuitePerl" => Some(NotQuitePerl),
            | "NuGet Config"
                | "NuGetConfig" => Some(NuGetConfig),
            | "Nushell" => Some(Nushell),
            | "Objective-C"
                | "ObjectiveC" => Some(ObjectiveC),
            | "Objective-C++"
                | "ObjectiveCpp" => Some(ObjectiveCpp),
            | "OCaml" => Some(OCaml),
            | "Odin" => Some(Odin),
            | "OpenSCAD"
                | "OpenScad" => Some(OpenScad),
            | "Open Policy Agent"
                | "OpenPolicyAgent" => Some(OpenPolicyAgent),
            | "OpenCL" => Some(OpenCL),
            | "OpenQASM"
                | "OpenQasm" => Some(OpenQasm),
            | "OpenType Feature File"
                | "OpenType" => Some(OpenType),
            | "Org" => Some(Org),
            | "Oz" => Some(Oz),
            | "Pacman's makepkg"
                | "PacmanMakepkg" => Some(PacmanMakepkg),
            | "Pan" => Some(Pan),
            | "Pascal" => Some(Pascal),
            | "Perl" => Some(Perl),
            | "Pest" => Some(Pest),
            | "Phix" => Some(Phix),
            | "PHP"
                | "Php" => Some(Php),
            | "PlantUML"
                | "PlantUml" => Some(PlantUml),
            | "PO File"
                | "Po" => Some(Po),
            | "Poke" => Some(Poke),
            | "Polly" => Some(Polly),
            | "Pony" => Some(Pony),
            | "PostCSS"
                | "PostCss" => Some(PostCss),
            | "PowerShell" => Some(PowerShell),
            | "Lauterbach PRACTICE Script"
                | "PRACTICE" => Some(PRACTICE),
            | "Processing" => Some(Processing),
            | "Prolog" => Some(Prolog),
            | "PSL Assertion"
                | "PSL" => Some(PSL),
            | "Protocol Buffers"
                | "Protobuf" => Some(Protobuf),
            | "Pug" => Some(Pug),
            | "Puppet" => Some(Puppet),
            | "PureScript" => Some(PureScript),
            | "Pyret" => Some(Pyret),
            | "Python" => Some(Python),
            | "PRQL" => Some(PRQL),
            | "Q" => Some(Q),
            | "QCL"
                | "Qcl" => Some(Qcl),
            | "QML"
                | "Qml" => Some(Qml),
            | "R" => Some(R),
            | "Racket" => Some(Racket),
            | "Rakefile" => Some(Rakefile),
            | "Raku" => Some(Raku),
            | "Razor" => Some(Razor),
            | "Redscript" => Some(Redscript),
            | "Ren'Py"
                | "Renpy" => Some(Renpy),
            | "ReScript" => Some(ReScript),
            | "ReStructuredText" => Some(ReStructuredText),
            | "Roc" => Some(Roc),
            | "Rusty Object Notation"
                | "RON" => Some(RON),
            | "RPM Specfile"
                | "RPMSpecfile" => Some(RPMSpecfile),
            | "Ruby" => Some(Ruby),
            | "Ruby HTML"
                | "RubyHtml" => Some(RubyHtml),
            | "Rust" => Some(Rust),
            | "Sass" => Some(Sass),
            | "Scala" => Some(Scala),
            | "Scheme" => Some(Scheme),
            | "Scons" => Some(Scons),
            | "Shell"
                | "Sh" => Some(Sh),
            | "ShaderLab" => Some(ShaderLab),
            | "SIL" => Some(SIL),
            | "Slang" => Some(Slang),
            | "Standard ML (SML)"
                | "Sml" => Some(Sml),
            | "Smalltalk" => Some(Smalltalk),
            | "Snakemake" => Some(Snakemake),
            | "Solidity" => Some(Solidity),
            | "Specman e"
                | "SpecmanE" => Some(SpecmanE),
            | "Spice Netlist"
                | "Spice" => Some(Spice),
            | "SQL"
                | "Sql" => Some(Sql),
            | "SQF"
                | "Sqf" => Some(Sqf),
            | "SRecode Template"
                | "SRecode" => Some(SRecode),
            | "Stan" => Some(Stan),
            | "Stata" => Some(Stata),
            | "Stratego/XT"
                | "Stratego" => Some(Stratego),
            | "Stylus" => Some(Stylus),
            | "Svelte" => Some(Svelte),
            | "SVG"
                | "Svg" => Some(Svg),
            | "Swift" => Some(Swift),
            | "SWIG"
                | "Swig" => Some(Swig),
            | "SystemVerilog" => Some(SystemVerilog),
            | "Slint" => Some(Slint),
            | "Tact" => Some(Tact),
            | "TCL"
                | "Tcl" => Some(Tcl),
            | "Tera" => Some(Tera),
            | "Templ" => Some(Templ),
            | "TeX"
                | "Tex" => Some(Tex),
            | "Plain Text"
                | "Text" => Some(Text),
            | "Thrift" => Some(Thrift),
            | "TOML"
                | "Toml" => Some(Toml),
            | "TSX"
                | "Tsx" => Some(Tsx),
            | "TTCN-3"
                | "Ttcn" => Some(Ttcn),
            | "Twig" => Some(Twig),
            | "TypeScript" => Some(TypeScript),
            | "Typst" => Some(Typst),
            | "Uiua" => Some(Uiua),
            | "UMPL" => Some(UMPL),
            | "Unison" => Some(Unison),
            | "Unreal Markdown"
                | "UnrealDeveloperMarkdown" => Some(UnrealDeveloperMarkdown),
            | "Unreal Plugin"
                | "UnrealPlugin" => Some(UnrealPlugin),
            | "Unreal Project"
                | "UnrealProject" => Some(UnrealProject),
            | "Unreal Script"
                | "UnrealScript" => Some(UnrealScript),
            | "Unreal Shader"
                | "UnrealShader" => Some(UnrealShader),
            | "Unreal Shader Header"
                | "UnrealShaderHeader" => Some(UnrealShaderHeader),
            | "Ur/Web"
                | "UrWeb" => Some(UrWeb),
            | "Ur/Web Project"
                | "UrWebProject" => Some(UrWebProject),
            | "Vala" => Some(Vala),
            | "VB6/VBA"
                | "VB6" => Some(VB6),
            | "VBScript" => Some(VBScript),
            | "Apache Velocity"
                | "Velocity" => Some(Velocity),
            | "Verilog" => Some(Verilog),
            | "Verilog Args File"
                | "VerilogArgsFile" => Some(VerilogArgsFile),
            | "VHDL"
                | "Vhdl" => Some(Vhdl),
            | "Virgil" => Some(Virgil),
            | "Visual Basic"
                | "VisualBasic" => Some(VisualBasic),
            | "Visual Studio Project"
                | "VisualStudioProject" => Some(VisualStudioProject),
            | "Visual Studio Solution"
                | "VisualStudioSolution" => Some(VisualStudioSolution),
            | "Vim Script"
                | "VimScript" => Some(VimScript),
            | "Vue" => Some(Vue),
            | "WebAssembly" => Some(WebAssembly),
            | "The WenYan Programming Language"
                | "WenYan" => Some(WenYan),
            | "WebGPU Shader Language"
                | "WGSL" => Some(WGSL),
            | "Wolfram" => Some(Wolfram),
            | "XAML"
                | "Xaml" => Some(Xaml),
            | "Xcode Config"
                | "XcodeConfig" => Some(XcodeConfig),
            | "XML"
                | "Xml" => Some(Xml),
            | "XSL" => Some(XSL),
            | "Xtend" => Some(Xtend),
            | "YAML"
                | "Yaml" => Some(Yaml),
            | "ZenCode" => Some(ZenCode),
            | "Zig" => Some(Zig),
            | "ZoKrates"
                | "Zokrates" => Some(Zokrates),
            | "Zsh" => Some(Zsh),
            | "GDShader"
                | "GdShader" => Some(GdShader),
            
            unknown => {
                warn!("Unknown language name: {}", unknown);
                None
            },
        }
    }

    /// Get language from its MIME type if available.
    ///
    /// ```no_run
    /// use tokei::LanguageType;
    ///
    /// let lang = LanguageType::from_mime("application/javascript");
    ///
    /// assert_eq!(lang, Some(LanguageType::JavaScript));
    /// ```
    #[must_use]
    pub fn from_mime(mime: &str) -> Option<Self> {
        match mime {
            | "text/css" => Some(Css),
                | "text/html" => Some(Html),
                | "application/javascript" | "application/ecmascript" | "application/x-ecmascript" | "application/x-javascript" | "text/javascript" | "text/ecmascript" | "text/javascript1.0" | "text/javascript1.1" | "text/javascript1.2" | "text/javascript1.3" | "text/javascript1.4" | "text/javascript1.5" | "text/jscript" | "text/livescript" | "text/x-ecmascript" | "text/x-javascript" => Some(JavaScript),
                | "application/json" | "application/manifest+json" => Some(Json),
                | "text/x-python" => Some(Python),
                | "application/prql" => Some(PRQL),
                | "image/svg+xml" => Some(Svg),
                | "text/plain" => Some(Text),
                
            _ => {
                warn!("Unknown MIME: {}", mime);
                None
            },
        }
    }

    /// Get language from a shebang. May open and read the file.
    ///
    /// ```no_run
    /// use tokei::LanguageType;
    ///
    /// let rust = LanguageType::from_shebang("./main.rs");
    ///
    /// assert_eq!(rust, Some(LanguageType::Rust));
    /// ```
    pub fn from_shebang<P: AsRef<Path>>(entry: P) -> Option<Self> {
        // Read at max `READ_LIMIT` bytes from the given file.
        // A typical shebang line has a length less than 32 characters;
        // e.g. '#!/bin/bash' - 11B / `#!/usr/bin/env python3` - 22B
        // It is *very* unlikely the file contains a valid shebang syntax
        // if we don't find a newline character after searching the first 128B.
        const READ_LIMIT: usize = 128;

        let mut file = File::open(entry).ok()?;
        let mut buf = [0; READ_LIMIT];

        let len = file.read(&mut buf).ok()?;
        let buf = &buf[..len];

        let first_line = buf.split(|b| *b == b'\n').next()?;
        let first_line = std::str::from_utf8(first_line).ok()?;

        let mut words = first_line.split_whitespace();
        match words.next() {
            
            | Some("#!/bin/awk -f") => Some(AWK),
                | Some("#!/bin/bash") => Some(Bash),
                | Some("#!/usr/bin/crystal") => Some(Crystal),
                | Some("#!/bin/csh") => Some(CShell),
                | Some("#!/bin/fish") => Some(Fish),
                | Some("#!/usr/bin/env just --justfile") => Some(Just),
                | Some("#!/bin/ksh") => Some(Ksh),
                | Some("#!/usr/bin/perl") => Some(Perl),
                | Some("#!/usr/bin/raku") | Some("#!/usr/bin/perl6") => Some(Raku),
                | Some("#!/bin/sh") => Some(Sh),
                | Some("#!/bin/zsh") => Some(Zsh),
                

            Some("#!/usr/bin/env") => {
                if let Some(word) = words.next() {
                    match word {
                        
                                    
                                        _ if word.starts_with("bash")
                                    
                                => Some(Bash),
                            
                                    
                                        _ if word.starts_with("crystal")
                                    
                                => Some(Crystal),
                            
                                    
                                        _ if word.starts_with("csh")
                                    
                                => Some(CShell),
                            
                                    
                                        _ if word.starts_with("cython")
                                    
                                => Some(Cython),
                            
                                    
                                        _ if word.starts_with("elvish")
                                    
                                => Some(Elvish),
                            
                                    
                                        _ if word.starts_with("fish")
                                    
                                => Some(Fish),
                            
                                    
                                        _ if word.starts_with("groovy")
                                    
                                => Some(Groovy),
                            
                                    
                                        _ if word.starts_with("just")
                                    
                                => Some(Just),
                            
                                    
                                        _ if word.starts_with("ksh")
                                    
                                => Some(Ksh),
                            
                                    
                                        _ if word.starts_with("python")
                                    
                                
                                    
                                        || word.starts_with("python2")
                                    
                                
                                    
                                        || word.starts_with("python3")
                                    
                                => Some(Python),
                            
                                    
                                        _ if word.starts_with("racket")
                                    
                                => Some(Racket),
                            
                                    
                                        _ if word.starts_with("raku")
                                    
                                
                                    
                                        || word.starts_with("perl6")
                                    
                                => Some(Raku),
                            
                                    
                                        _ if word.starts_with("ruby")
                                    
                                => Some(Ruby),
                            
                                    
                                        _ if word.starts_with("sh")
                                    
                                => Some(Sh),
                            
                        env => {
                            warn!("Unknown environment: {:?}", env);
                            None
                        }
                    }
                } else {
                    None
                }
            }
            _ => None,
        }
    }
}

impl FromStr for LanguageType {
    type Err = &'static str;

    fn from_str(from: &str) -> Result<Self, Self::Err> {
        match &*from.to_lowercase() {
            
                "abap"
                => Ok(Abap),
            
                "abnf"
                => Ok(ABNF),
            
                "actionscript"
                => Ok(ActionScript),
            
                "ada"
                => Ok(Ada),
            
                "agda"
                => Ok(Agda),
            
                "alex"
                => Ok(Alex),
            
                "alloy"
                => Ok(Alloy),
            
                "apl"
                => Ok(Apl),
            
                "arduino c++"
                => Ok(Arduino),
            
                "ark typescript"
                => Ok(ArkTS),
            
                "arturo"
                => Ok(Arturo),
            
                "asciidoc"
                => Ok(AsciiDoc),
            
                "asn.1"
                => Ok(Asn1),
            
                "asp"
                => Ok(Asp),
            
                "asp.net"
                => Ok(AspNet),
            
                "assembly"
                => Ok(Assembly),
            
                "gnu style assembly"
                => Ok(AssemblyGAS),
            
                "astro"
                => Ok(Astro),
            
                "ats"
                => Ok(Ats),
            
                "autoconf"
                => Ok(Autoconf),
            
                "autoit"
                => Ok(Autoit),
            
                "autohotkey"
                => Ok(AutoHotKey),
            
                "automake"
                => Ok(Automake),
            
                "axaml"
                => Ok(AvaloniaXaml),
            
                "awk"
                => Ok(AWK),
            
                "ballerina"
                => Ok(Ballerina),
            
                "bash"
                => Ok(Bash),
            
                "batch"
                => Ok(Batch),
            
                "bazel"
                => Ok(Bazel),
            
                "bean"
                => Ok(Bean),
            
                "bicep"
                => Ok(Bicep),
            
                "bitbake"
                => Ok(Bitbake),
            
                "bqn"
                => Ok(Bqn),
            
                "brightscript"
                => Ok(BrightScript),
            
                "c"
                => Ok(C),
            
                "cabal"
                => Ok(Cabal),
            
                "cairo"
                => Ok(Cairo),
            
                "cangjie"
                => Ok(Cangjie),
            
                "cassius"
                => Ok(Cassius),
            
                "ceylon"
                => Ok(Ceylon),
            
                "chapel"
                => Ok(Chapel),
            
                "c header"
                => Ok(CHeader),
            
                "cil (selinux)"
                => Ok(Cil),
            
                "circom"
                => Ok(Circom),
            
                "clojure"
                => Ok(Clojure),
            
                "clojurec"
                => Ok(ClojureC),
            
                "clojurescript"
                => Ok(ClojureScript),
            
                "cmake"
                => Ok(CMake),
            
                "cobol"
                => Ok(Cobol),
            
                "codeql"
                => Ok(CodeQL),
            
                "coffeescript"
                => Ok(CoffeeScript),
            
                "cogent"
                => Ok(Cogent),
            
                "coldfusion"
                => Ok(ColdFusion),
            
                "coldfusion cfscript"
                => Ok(ColdFusionScript),
            
                "coq"
                => Ok(Coq),
            
                "c++"
                => Ok(Cpp),
            
                "c++ header"
                => Ok(CppHeader),
            
                "c++ module"
                => Ok(CppModule),
            
                "crystal"
                => Ok(Crystal),
            
                "c#"
                => Ok(CSharp),
            
                "c shell"
                => Ok(CShell),
            
                "css"
                => Ok(Css),
            
                "cuda"
                => Ok(Cuda),
            
                "cue"
                => Ok(Cue),
            
                "cython"
                => Ok(Cython),
            
                "d"
                => Ok(D),
            
                "d2"
                => Ok(D2),
            
                "daml"
                => Ok(Daml),
            
                "dart"
                => Ok(Dart),
            
                "device tree"
                => Ok(DeviceTree),
            
                "dhall"
                => Ok(Dhall),
            
                "dockerfile"
                => Ok(Dockerfile),
            
                ".net resource"
                => Ok(DotNetResource),
            
                "dream maker"
                => Ok(DreamMaker),
            
                "dust.js"
                => Ok(Dust),
            
                "ebuild"
                => Ok(Ebuild),
            
                "edgeql"
                => Ok(EdgeQL),
            
                "edgedb schema definition"
                => Ok(ESDL),
            
                "edn"
                => Ok(Edn),
            
                "8th"
                => Ok(Eighth),
            
                "emacs lisp"
                => Ok(Elisp),
            
                "elixir"
                => Ok(Elixir),
            
                "elm"
                => Ok(Elm),
            
                "elvish"
                => Ok(Elvish),
            
                "emacs dev env"
                => Ok(EmacsDevEnv),
            
                "emojicode"
                => Ok(Emojicode),
            
                "erlang"
                => Ok(Erlang),
            
                "factor"
                => Ok(Factor),
            
                "fen"
                => Ok(FEN),
            
                "fennel"
                => Ok(Fennel),
            
                "fish"
                => Ok(Fish),
            
                "flatbuffers schema"
                => Ok(FlatBuffers),
            
                "forge config"
                => Ok(ForgeConfig),
            
                "forth"
                => Ok(Forth),
            
                "fortran legacy"
                => Ok(FortranLegacy),
            
                "fortran modern"
                => Ok(FortranModern),
            
                "freemarker"
                => Ok(FreeMarker),
            
                "f#"
                => Ok(FSharp),
            
                "f*"
                => Ok(Fstar),
            
                "futhark"
                => Ok(Futhark),
            
                "gdb script"
                => Ok(GDB),
            
                "gdscript"
                => Ok(GdScript),
            
                "gherkin (cucumber)"
                => Ok(Gherkin),
            
                "gleam"
                => Ok(Gleam),
            
                "glimmer js"
                => Ok(GlimmerJs),
            
                "glimmer ts"
                => Ok(GlimmerTs),
            
                "glsl"
                => Ok(Glsl),
            
                "gml"
                => Ok(Gml),
            
                "go"
                => Ok(Go),
            
                "go html"
                => Ok(Gohtml),
            
                "graphql"
                => Ok(Graphql),
            
                "groovy"
                => Ok(Groovy),
            
                "gwion"
                => Ok(Gwion),
            
                "haml"
                => Ok(Haml),
            
                "hamlet"
                => Ok(Hamlet),
            
                "happy"
                => Ok(Happy),
            
                "handlebars"
                => Ok(Handlebars),
            
                "haskell"
                => Ok(Haskell),
            
                "haxe"
                => Ok(Haxe),
            
                "hcl"
                => Ok(Hcl),
            
                "headache"
                => Ok(Headache),
            
                "hex"
                => Ok(Hex),
            
                "hex0"
                => Ok(Hex0),
            
                "hex1"
                => Ok(Hex1),
            
                "hex2"
                => Ok(Hex2),
            
                "hicad"
                => Ok(HiCad),
            
                "hlsl"
                => Ok(Hlsl),
            
                "holyc"
                => Ok(HolyC),
            
                "html"
                => Ok(Html),
            
                "hy"
                => Ok(Hy),
            
                "idris"
                => Ok(Idris),
            
                "ini"
                => Ok(Ini),
            
                "intel hex"
                => Ok(IntelHex),
            
                "isabelle"
                => Ok(Isabelle),
            
                "jai"
                => Ok(Jai),
            
                "janet"
                => Ok(Janet),
            
                "java"
                => Ok(Java),
            
                "javascript"
                => Ok(JavaScript),
            
                "jinja2"
                => Ok(Jinja2),
            
                "jq"
                => Ok(Jq),
            
                "jslt"
                => Ok(JSLT),
            
                "json"
                => Ok(Json),
            
                "jsonnet"
                => Ok(Jsonnet),
            
                "jsx"
                => Ok(Jsx),
            
                "julia"
                => Ok(Julia),
            
                "julius"
                => Ok(Julius),
            
                "jupyter notebooks"
                => Ok(Jupyter),
            
                "just"
                => Ok(Just),
            
                "k"
                => Ok(K),
            
                "kakoune script"
                => Ok(KakouneScript),
            
                "kaem"
                => Ok(Kaem),
            
                "koka"
                => Ok(Koka),
            
                "kotlin"
                => Ok(Kotlin),
            
                "korn shell"
                => Ok(Ksh),
            
                "lalrpop"
                => Ok(Lalrpop),
            
                "kv language"
                => Ok(KvLanguage),
            
                "lean"
                => Ok(Lean),
            
                "hledger"
                => Ok(Hledger),
            
                "less"
                => Ok(Less),
            
                "lex"
                => Ok(Lex),
            
                "liquid"
                => Ok(Liquid),
            
                "lingua franca"
                => Ok(LinguaFranca),
            
                "ld script"
                => Ok(LinkerScript),
            
                "common lisp"
                => Ok(Lisp),
            
                "livescript"
                => Ok(LiveScript),
            
                "llvm"
                => Ok(LLVM),
            
                "logtalk"
                => Ok(Logtalk),
            
                "lolcode"
                => Ok(LolCode),
            
                "lua"
                => Ok(Lua),
            
                "lucius"
                => Ok(Lucius),
            
                "m1 assembly"
                => Ok(M1Assembly),
            
                "m4"
                => Ok(M4),
            
                "madlang"
                => Ok(Madlang),
            
                "makefile"
                => Ok(Makefile),
            
                "markdown"
                => Ok(Markdown),
            
                "max"
                => Ok(Max),
            
                "mdx"
                => Ok(Mdx),
            
                "menhir"
                => Ok(Menhir),
            
                "meson"
                => Ok(Meson),
            
                "metal shading language"
                => Ok(Metal),
            
                "mint"
                => Ok(Mint),
            
                "mlatu"
                => Ok(Mlatu),
            
                "modelica"
                => Ok(Modelica),
            
                "module-definition"
                => Ok(ModuleDef),
            
                "mojo"
                => Ok(Mojo),
            
                "monkey c"
                => Ok(MonkeyC),
            
                "moonbit"
                => Ok(MoonBit),
            
                "moonscript"
                => Ok(MoonScript),
            
                "msbuild"
                => Ok(MsBuild),
            
                "mustache"
                => Ok(Mustache),
            
                "nextflow"
                => Ok(Nextflow),
            
                "nim"
                => Ok(Nim),
            
                "nix"
                => Ok(Nix),
            
                "not quite perl"
                => Ok(NotQuitePerl),
            
                "nuget config"
                => Ok(NuGetConfig),
            
                "nushell"
                => Ok(Nushell),
            
                "objective-c"
                => Ok(ObjectiveC),
            
                "objective-c++"
                => Ok(ObjectiveCpp),
            
                "ocaml"
                => Ok(OCaml),
            
                "odin"
                => Ok(Odin),
            
                "openscad"
                => Ok(OpenScad),
            
                "open policy agent"
                => Ok(OpenPolicyAgent),
            
                "opencl"
                => Ok(OpenCL),
            
                "openqasm"
                => Ok(OpenQasm),
            
                "opentype feature file"
                => Ok(OpenType),
            
                "org"
                => Ok(Org),
            
                "oz"
                => Ok(Oz),
            
                "pacman's makepkg"
                => Ok(PacmanMakepkg),
            
                "pan"
                => Ok(Pan),
            
                "pascal"
                => Ok(Pascal),
            
                "perl"
                => Ok(Perl),
            
                "pest"
                => Ok(Pest),
            
                "phix"
                => Ok(Phix),
            
                "php"
                => Ok(Php),
            
                "plantuml"
                => Ok(PlantUml),
            
                "po file"
                => Ok(Po),
            
                "poke"
                => Ok(Poke),
            
                "polly"
                => Ok(Polly),
            
                "pony"
                => Ok(Pony),
            
                "postcss"
                => Ok(PostCss),
            
                "powershell"
                => Ok(PowerShell),
            
                "lauterbach practice script"
                => Ok(PRACTICE),
            
                "processing"
                => Ok(Processing),
            
                "prolog"
                => Ok(Prolog),
            
                "psl assertion"
                => Ok(PSL),
            
                "protocol buffers"
                => Ok(Protobuf),
            
                "pug"
                => Ok(Pug),
            
                "puppet"
                => Ok(Puppet),
            
                "purescript"
                => Ok(PureScript),
            
                "pyret"
                => Ok(Pyret),
            
                "python"
                => Ok(Python),
            
                "prql"
                => Ok(PRQL),
            
                "q"
                => Ok(Q),
            
                "qcl"
                => Ok(Qcl),
            
                "qml"
                => Ok(Qml),
            
                "r"
                => Ok(R),
            
                "racket"
                => Ok(Racket),
            
                "rakefile"
                => Ok(Rakefile),
            
                "raku"
                => Ok(Raku),
            
                "razor"
                => Ok(Razor),
            
                "redscript"
                => Ok(Redscript),
            
                "ren'py"
                => Ok(Renpy),
            
                "rescript"
                => Ok(ReScript),
            
                "restructuredtext"
                => Ok(ReStructuredText),
            
                "roc"
                => Ok(Roc),
            
                "rusty object notation"
                => Ok(RON),
            
                "rpm specfile"
                => Ok(RPMSpecfile),
            
                "ruby"
                => Ok(Ruby),
            
                "ruby html"
                => Ok(RubyHtml),
            
                "rust"
                => Ok(Rust),
            
                "sass"
                => Ok(Sass),
            
                "scala"
                => Ok(Scala),
            
                "scheme"
                => Ok(Scheme),
            
                "scons"
                => Ok(Scons),
            
                "shell"
                => Ok(Sh),
            
                "shaderlab"
                => Ok(ShaderLab),
            
                "sil"
                => Ok(SIL),
            
                "slang"
                => Ok(Slang),
            
                "standard ml (sml)"
                => Ok(Sml),
            
                "smalltalk"
                => Ok(Smalltalk),
            
                "snakemake"
                => Ok(Snakemake),
            
                "solidity"
                => Ok(Solidity),
            
                "specman e"
                => Ok(SpecmanE),
            
                "spice netlist"
                => Ok(Spice),
            
                "sql"
                => Ok(Sql),
            
                "sqf"
                => Ok(Sqf),
            
                "srecode template"
                => Ok(SRecode),
            
                "stan"
                => Ok(Stan),
            
                "stata"
                => Ok(Stata),
            
                "stratego/xt"
                => Ok(Stratego),
            
                "stylus"
                => Ok(Stylus),
            
                "svelte"
                => Ok(Svelte),
            
                "svg"
                => Ok(Svg),
            
                "swift"
                => Ok(Swift),
            
                "swig"
                => Ok(Swig),
            
                "systemverilog"
                => Ok(SystemVerilog),
            
                "slint"
                => Ok(Slint),
            
                "tact"
                => Ok(Tact),
            
                "tcl"
                => Ok(Tcl),
            
                "tera"
                => Ok(Tera),
            
                "templ"
                => Ok(Templ),
            
                "tex"
                => Ok(Tex),
            
                "plain text"
                => Ok(Text),
            
                "thrift"
                => Ok(Thrift),
            
                "toml"
                => Ok(Toml),
            
                "tsx"
                => Ok(Tsx),
            
                "ttcn-3"
                => Ok(Ttcn),
            
                "twig"
                => Ok(Twig),
            
                "typescript"
                => Ok(TypeScript),
            
                "typst"
                => Ok(Typst),
            
                "uiua"
                => Ok(Uiua),
            
                "umpl"
                => Ok(UMPL),
            
                "unison"
                => Ok(Unison),
            
                "unreal markdown"
                => Ok(UnrealDeveloperMarkdown),
            
                "unreal plugin"
                => Ok(UnrealPlugin),
            
                "unreal project"
                => Ok(UnrealProject),
            
                "unreal script"
                => Ok(UnrealScript),
            
                "unreal shader"
                => Ok(UnrealShader),
            
                "unreal shader header"
                => Ok(UnrealShaderHeader),
            
                "ur/web"
                => Ok(UrWeb),
            
                "ur/web project"
                => Ok(UrWebProject),
            
                "vala"
                => Ok(Vala),
            
                "vb6/vba"
                => Ok(VB6),
            
                "vbscript"
                => Ok(VBScript),
            
                "apache velocity"
                => Ok(Velocity),
            
                "verilog"
                => Ok(Verilog),
            
                "verilog args file"
                => Ok(VerilogArgsFile),
            
                "vhdl"
                => Ok(Vhdl),
            
                "virgil"
                => Ok(Virgil),
            
                "visual basic"
                => Ok(VisualBasic),
            
                "visual studio project"
                => Ok(VisualStudioProject),
            
                "visual studio solution"
                => Ok(VisualStudioSolution),
            
                "vim script"
                => Ok(VimScript),
            
                "vue"
                => Ok(Vue),
            
                "webassembly"
                => Ok(WebAssembly),
            
                "the wenyan programming language"
                => Ok(WenYan),
            
                "webgpu shader language"
                => Ok(WGSL),
            
                "wolfram"
                => Ok(Wolfram),
            
                "xaml"
                => Ok(Xaml),
            
                "xcode config"
                => Ok(XcodeConfig),
            
                "xml"
                => Ok(Xml),
            
                "xsl"
                => Ok(XSL),
            
                "xtend"
                => Ok(Xtend),
            
                "yaml"
                => Ok(Yaml),
            
                "zencode"
                => Ok(ZenCode),
            
                "zig"
                => Ok(Zig),
            
                "zokrates"
                => Ok(Zokrates),
            
                "zsh"
                => Ok(Zsh),
            
                "gdshader"
                => Ok(GdShader),
            
            _ => Err("Language not found, please use `-l` to see all available\
                     languages."),
        }
    }
}

impl fmt::Display for LanguageType {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.name())
    }
}


impl<'a> From<LanguageType> for Cow<'a, LanguageType> {
    fn from(from: LanguageType) -> Self {
        Cow::Owned(from)
    }
}

impl<'a> From<&'a LanguageType> for Cow<'a, LanguageType> {
    fn from(from: &'a LanguageType) -> Self {
        Cow::Borrowed(from)
    }
}
