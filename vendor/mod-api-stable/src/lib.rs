//! # mod-api-stable
//!
//! Stable-ABI modding contract for TeamfightManager2.
//! Design doc: `docs/mod_stable_abi_design.md`.
//!
//! A mod DLL built against this crate once keeps loading on future game
//! builds. That guarantee holds because of four boundary rules:
//!
//! 1. Only `#[repr(C)]` POD value structs, opaque `u64` handles, C function
//!    pointers, and `(ptr, len)` slices cross the DLL boundary. No Rust
//!    `String`/`Vec`/`Box<dyn>`, and no game-internal types — ever.
//! 2. Function tables and boundary structs are append-only. Every one starts
//!    with a `size` field written by whichever side filled it; readers only
//!    touch fields that fit inside that size.
//! 3. Published value types are frozen. Extension means a new `*V2` type plus
//!    new slots, never editing a `*V1`.
//! 4. Versioning is a single monotonically increasing [`ABI_LEVEL`]. The
//!    loader accepts a mod when its required level <= the host level. A mod
//!    declares [`BASELINE_ABI_LEVEL`] by default — rule 2 already makes every
//!    individual call degrade on an older host, so building against a newer
//!    SDK must not cost backward compatibility. Raising the requirement is an
//!    explicit opt-in for mods that are inert without a newer host.
//!
//! The contract is enforced by snapshot tests in `tests/contract_guard.rs`
//! against `contract/abi_v1.txt`.

/// (crate-internal) Calls a table slot only when it exists inside the size
/// the other side declared — the mechanism that lets mixed-level builds
/// cooperate. Defined at the root so every wrapper module shares it.
macro_rules! slot {
    ($table_ptr:expr, $table_ty:ident, $slot:ident) => {{
        let table = $table_ptr;
        if table.is_null() {
            None
        } else {
            let table_size = unsafe { (*table).size };
            let end = std::mem::offset_of!($table_ty, $slot)
                + std::mem::size_of::<Option<unsafe extern "C" fn()>>();
            if end <= table_size {
                unsafe { (*table).$slot }
            } else {
                None
            }
        }
    }};
}

mod ai;
mod ai_ctx;
mod client;
mod client_ctx;
mod contract;
mod draw;
mod entry;
mod export;
mod gameplay;
mod handle;
mod host;
mod json_doc;
mod json_help;
mod mod_shim;
mod server;
mod server_ctx;
mod sim;
mod traits;
mod values;
mod wrapper;

pub use ai::{
    AiCtxV1, AiInitV1, AiVtableV1, DraftCtxV1, DraftHookRegV1, DraftHookVtableV1, ItemBuildCtxV1,
    ItemBuildHookRegV1, ItemBuildHookVtableV1, PlayerAiRegV1, PlayerAiVtableV1,
};
pub use ai_ctx::{
    StableAiContext, StableAiInit, StableDraftContext, StableDraftDecision, StableItemBuildContext,
};
pub use client::{ClientCtxV1, ExtensionRegV1, ExtensionVtableV1, UiClickHandlerFn};
pub use client_ctx::{
    StableChampionBrief, StableClient, StableInputEvent, StableModEvent, StableSpriteParams,
    StableUiEvent,
};
pub use contract::contract_dump;
pub use draw::DrawVtableV1;
pub use entry::{
    entry_shim, resolve_required_level, ModRequiredAbiLevelFn, ModStableEntryFn,
    BASELINE_ABI_LEVEL, MOD_REQUIRED_ABI_LEVEL_SYMBOL, MOD_STABLE_ENTRY_SYMBOL,
};
pub use export::ModExportV1;
pub use gameplay::{
    ActionRegV1, ActionVtableV1, ChampionRegV1, ChampionVtableV1, EffectSpecV1, EffectTypeRegV1,
    EffectTypeVtableV1, ItemRegV1, ItemVtableV1, MatchHookRegV1, MatchHookVtableV1,
    NativeEffectRegV1, NativePassiveRegV1, PassiveRegV1, PassiveVtableV1, SimCtxV1,
};
pub use handle::{EntityHandleV1, PlayerHandleV1, ProjectileHandleV1};
pub use json_doc::{
    JsonDocCtxV1, JsonDocVtableV1, MapCustomizerRegV1, MapCustomizerVtableV1, StableJsonDoc,
};
pub use host::{
    AssetVtableV1, DataVtableV1, FrameVtableV1, GameVersionV1, HostApiV1, ModServiceV1,
    NetVtableV1, SaveVtableV1, SceneVtableV1, ServiceVersionV1, ServiceVtableV1, SimVtableV1,
    UiVtableV1, LOG_LEVEL_DEBUG, LOG_LEVEL_ERROR, LOG_LEVEL_INFO, LOG_LEVEL_WARN,
};
pub use server::{ServerCtxV1, ServerExtRegV1, ServerExtVtableV1, ServerVtableV1};
pub use server_ctx::{StableCommand, StableManagementEvent, StableServerCtx};
pub use sim::{StableEntity, StablePlayer, StableSim};
pub use traits::{
    StableAction, StableChampion, StableDraftHook, StableEffectSpec, StableEffectType,
    StableExtension, StableItem, StableItemBuildHook, StableMapCustomizer, StableMatchHook,
    StablePassive, StablePlayerAi, StableServerExtension,
};
pub use values::{
    AiDecisionKindV1, AttackTypeV1, BuffDurationV1, BuffV1, CastingTargetV1, CastingTypeV1,
    CcKindV1, CcV1, ChampionBriefV1, ChampionCategoryV1, ChampionTagV1, ClientSceneKindV1,
    CommandResultV1, DamageTypeV1, DifficultyV1, DraftDecisionKindV1, DraftPhaseV1,
    EventTargetKindV1,
    GameModeKindV1, InputEventKindV1, InputKindV1, InputTargetKindV1, InputTargetV1, InputV1,
    ItemCategoryV1, ItemTagV1, KillLogV1, LaneV1, ManagementEventKindV1, ProjectileInfoV1,
    ProjectileMoveKindV1,
    ProjectileSpawnV1, RecordKindV1, SceneKindV1, SettingTargetV1, StableEventTarget, StatV1,
    StrV1, TextAlignXV1, TextAlignYV1, UiEventKindV1, UnitAttackV1, BUFF_NAME_CAP,
    CHAMPION_BRIEF_TAG_CAP,
};
pub use wrapper::{LogLevel, StableHost, StableMod};
pub use values::{SimOriginKindV1, SimOriginV1};

/// Current ABI level of this crate. Bumped (+1) every time the host appends
/// slots/fields/types to the contract. Level 1 is the initial contract.
///
/// Level 2 appends the Support item category and
/// `ClientSceneKindV1::ChampionshipCelebration`, shield/attack-speed/raw-damage
/// sim slots, item/passive `*_ex` callbacks, item-build hooks, and champion
/// lane priors. Level 1 mods remain loadable through the size/code guards.
///
/// Levels 3 and 4 were handed out independently in the two lineages before they
/// merged, so level 5 is the first level whose contract holds all of them:
/// - `ClientSceneKindV1::AwardsCeremony` (beta lineage, was level 3)
/// - the read-only `RecordKindV1::MatchReplay` table, letting mods read match
///   history without a classic-API dependency (main lineage, was level 3)
/// - the `UiVtableV1` image-fill slots `set_team_logo` / `set_champion_icon`,
///   exposing the game's own logo/icon resolution (save-embedded custom logos,
///   champion face crops) to image nodes mods own (main lineage, was level 4)
///
/// Mods built against level 3 or 4 of either lineage stay loadable: every entry
/// above is appended, never renumbered, and the size/code guards cover the rest.
///
/// Level 6 appends the gaps modders reported once the classic SDK closed:
/// `SimVtableV1` `play_view_effect` / `play_sfx` (VFX and sound at a position or
/// entity), `entity_set_invisible` / `entity_banish` (the invisible and banish
/// primitives `.data_champion` already had), `deal_damage_typed` (true damage
/// through the real attack pipeline) and `sim_origin` (is this simulation the
/// match on screen, a replay, or server pre-sim — with match/replay/set ids);
/// `DataVtableV1` `native_effect_count` / `native_effect_name_at` (the names
/// `queue_effect` and `Native { effect_ref }` accept).
///
/// Level 7 appends caster-relative displacement to `SimVtableV1`:
/// `entity_knockback` (knock the target away from the caster), `entity_grab`
/// (pull the target to the caster; `ticks == 0` = until reached) and
/// `entity_pull` (drag the target toward the caster) — the `.data_champion`
/// `Knockback` / `Grab` / `Pull` primitives, exposed to native mods.
///
/// Level 8 was handed out in both lineages and this branch holds the union:
/// `ClientSceneKindV1::SetFeedback` (between-set feedback screen: scene-open
/// timeline + replay; beta lineage) and `SimVtableV1` `entity_stack_buff` (one
/// call to grow a named-buff stack — counts same-name stat buffs, optionally
/// refreshes their remaining duration, and appends one more copy under a
/// `max_stacks` cap — replacing the read-remove-re-add loop stacking mods had
/// to hand-roll; main lineage).
///
/// Level 9 appends named native passive registration: `ModExportV1`
/// `native_passives_ptr/len` (`NativePassiveRegV1` entries from
/// `StableMod::add_native_passive`) and the `PassiveVtableV1` `configure`
/// slot — a mod-registered passive becomes attachable to `.data_champion`
/// files by name, with the file's integer params delivered as sorted-key
/// JSON. Data champions no longer need to go full native for a custom
/// passive.
pub const ABI_LEVEL: u32 = 9;
