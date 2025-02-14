// Copyright 2019-2022 Manta Network.
// This file is part of manta-rs.
//
// manta-rs is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// manta-rs is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with manta-rs.  If not, see <http://www.gnu.org/licenses/>.

//! Transfer Protocol
//!
//! This module defines a protocol for the zero-knowledge transfer of private assets. We define the
//! following structures:
//!
//! - Global Configuration: [`Configuration`]
//! - Sender Abstraction: [`Sender`], [`SenderPost`], [`SenderLedger`](
//! - Receiver Abstraction: [`Receiver`], [`ReceiverPost`], [`ReceiverLedger`]
//! - Transfer Abstraction: [`Transfer`], [`TransferPost`], [`TransferLedger`]
//! - Canonical Transactions: [`canonical`]
//! - Batched Transactions: [`batch`]
//!
//! See the [`crate::wallet`] module for more on how this transfer protocol is used in a wallet
//! protocol for the keeping of accounts for private assets.

use crate::{
    asset,
    transfer::{
        canonical::TransferShape,
        receiver::{ReceiverLedger, ReceiverPostError},
        sender::{SenderLedger, SenderPostError},
        utxo::{auth, Mint, NullifierIndependence, Spend, UtxoIndependence, UtxoReconstruct},
    },
};
use core::{fmt::Debug, hash::Hash, iter::Sum, ops::AddAssign};
use manta_crypto::{
    accumulator::{self, Accumulator, ItemHashFunction, MembershipProof, Model},
    constraint::{HasInput, Input, ProofSystem},
    eclair::{
        self,
        alloc::{
            mode::{Derived, Public, Secret},
            Allocate, Allocator, Constant, Var, Variable,
        },
        bool::{Assert, AssertEq},
        ops::Add,
    },
    rand::{CryptoRng, RngCore, Sample},
};
use manta_util::{
    cmp::Independence,
    codec::{Encode, Write},
    convert::Field,
    vec::{all_unequal, Vec},
};

#[cfg(feature = "serde")]
use manta_util::serde::{Deserialize, Serialize};

pub mod batch;
pub mod canonical;
pub mod receiver;
pub mod sender;
pub mod utxo;

#[cfg(feature = "test")]
#[cfg_attr(doc_cfg, doc(cfg(feature = "test")))]
pub mod test;

#[doc(inline)]
pub use canonical::Shape;

/// Returns `true` if the [`Transfer`] with this shape would have public participants.
#[inline]
pub const fn has_public_participants(sources: usize, sinks: usize) -> bool {
    (sources + sinks) > 0
}

/// Returns `true` if the [`Transfer`] with this shape would have secret participants.
#[inline]
pub const fn has_secret_participants(senders: usize, receivers: usize) -> bool {
    (senders + receivers) > 0
}

/// Returns `true` if the [`Transfer`] with this shape would require an authorization.
#[inline]
pub const fn requires_authorization(senders: usize) -> bool {
    senders > 0
}

/// Returns `true` if the [`Transfer`] with this shape has sinks.
#[inline]
pub const fn has_sinks(sinks: usize) -> bool {
    sinks > 0
}

/// Configuration
pub trait Configuration {
    /// Compiler Type
    type Compiler: Assert;

    /// Asset Id Type
    type AssetId: Clone + Ord;

    /// Asset Value Type
    type AssetValue: AddAssign + Clone + Default + PartialOrd + Sum;

    /// Account Identifier
    type AccountId: Clone;

    /// Associated Data Type
    type AssociatedData: Default;

    /// Unspent Transaction Output Type
    type Utxo: Independence<UtxoIndependence>;

    /// Nullifier Type
    type Nullifier: Independence<NullifierIndependence>;

    /// Identifier Type
    type Identifier: Clone + Sample;

    /// Address Type
    type Address: Clone;

    /// Note Type
    type Note: AsRef<BaseNote<Self>> + Into<BaseNote<Self>>;

    /// Mint Secret Type
    type MintSecret: utxo::QueryIdentifier<Identifier = Identifier<Self>, Utxo = Self::Utxo>;

    /// Spend Secret Type
    type SpendSecret: utxo::QueryAsset<Asset = Asset<Self>, Utxo = Self::Utxo>;

    /// [`Utxo`] Accumulator Witness Type
    type UtxoAccumulatorWitness: Default;

    /// [`Utxo`] Accumulator Output Type
    type UtxoAccumulatorOutput: Default;

    /// [`Utxo`] Accumulator Item Hash Type
    type UtxoAccumulatorItemHash: ItemHashFunction<Utxo<Self>, Item = UtxoAccumulatorItem<Self>>;

    /// Parameters Type
    type Parameters: Clone
        + auth::DeriveContext
        + auth::ProveAuthorization
        + auth::VerifyAuthorization
        + auth::DeriveSigningKey
        + for<'a> auth::Sign<BodyWithAccountsRef<'a, Self>>
        + for<'a> auth::VerifySignature<BodyWithAccountsRef<'a, Self>>
        + utxo::AssetType<Asset = Asset<Self>>
        + utxo::AssociatedDataType<AssociatedData = Self::AssociatedData>
        + utxo::DeriveMint<
            Secret = Self::MintSecret,
            Utxo = Self::Utxo,
            Address = Self::Address,
            Note = Self::Note,
        > + utxo::DeriveSpend<
            UtxoAccumulatorWitness = Self::UtxoAccumulatorWitness,
            UtxoAccumulatorOutput = Self::UtxoAccumulatorOutput,
            UtxoAccumulatorItemHash = Self::UtxoAccumulatorItemHash,
            Secret = Self::SpendSecret,
            Nullifier = Self::Nullifier,
            Identifier = Self::Identifier,
        > + utxo::UtxoReconstruct;

    /// Authorization Context Variable Type
    type AuthorizationContextVar: Variable<
        Secret,
        Self::Compiler,
        Type = AuthorizationContext<Self>,
    >;

    /// Authorization Proof Variable Type
    type AuthorizationProofVar: Variable<Derived, Self::Compiler, Type = AuthorizationProof<Self>>;

    /// Asset Id Variable Type
    type AssetIdVar: Variable<Secret, Self::Compiler, Type = Self::AssetId>
        + Variable<Public, Self::Compiler, Type = Self::AssetId>
        + eclair::cmp::PartialEq<Self::AssetIdVar, Self::Compiler>;

    /// Asset Value Variable Type
    type AssetValueVar: Variable<Secret, Self::Compiler, Type = Self::AssetValue>
        + Variable<Public, Self::Compiler, Type = Self::AssetValue>
        + Add<Self::AssetValueVar, Self::Compiler, Output = Self::AssetValueVar>
        + eclair::cmp::PartialEq<Self::AssetValueVar, Self::Compiler>;

    /// Unspent Transaction Output Variable Type
    type UtxoVar: Variable<Secret, Self::Compiler, Type = Self::Utxo>
        + Variable<Public, Self::Compiler, Type = Self::Utxo>;

    /// Note Variable Type
    type NoteVar: Variable<Public, Self::Compiler>;

    /// Nullifier Variable Type
    type NullifierVar: Variable<Public, Self::Compiler, Type = Self::Nullifier>;

    /// UTXO Accumulator Witness Variable Type
    type UtxoAccumulatorWitnessVar: Variable<
        Secret,
        Self::Compiler,
        Type = UtxoAccumulatorWitness<Self>,
    >;

    /// UTXO Accumulator Output Variable Type
    type UtxoAccumulatorOutputVar: Variable<
        Public,
        Self::Compiler,
        Type = UtxoAccumulatorOutput<Self>,
    >;

    /// UTXO Accumulator Model Variable Type
    type UtxoAccumulatorModelVar: Constant<Self::Compiler, Type = UtxoAccumulatorModel<Self>>
        + accumulator::Model<
            Self::Compiler,
            Witness = Self::UtxoAccumulatorWitnessVar,
            Output = Self::UtxoAccumulatorOutputVar,
        >;

    /// Mint Secret Variable Type
    type MintSecretVar: Variable<Secret, Self::Compiler, Type = <Self::Parameters as Mint>::Secret>;

    /// Spend Secret Variable Type
    type SpendSecretVar: Variable<
        Secret,
        Self::Compiler,
        Type = <Self::Parameters as Spend>::Secret,
    >;

    /// Parameters Variable Type
    type ParametersVar: Constant<Self::Compiler, Type = Self::Parameters>
        + auth::AssertAuthorized<
            Compiler<Self>,
            AuthorizationContext = Self::AuthorizationContextVar,
            AuthorizationProof = Self::AuthorizationProofVar,
        > + utxo::AssetType<Asset = AssetVar<Self>>
        + utxo::UtxoType<Utxo = Self::UtxoVar>
        + Mint<Self::Compiler, Secret = Self::MintSecretVar, Note = Self::NoteVar>
        + Spend<
            Self::Compiler,
            UtxoAccumulatorModel = Self::UtxoAccumulatorModelVar,
            Secret = Self::SpendSecretVar,
            Nullifier = Self::NullifierVar,
        >;

    /// Proof System Type
    type ProofSystem: ProofSystem<Compiler = Self::Compiler>
        + HasInput<AuthorizationKey<Self>>
        + HasInput<Self::AssetId>
        + HasInput<Self::AssetValue>
        + HasInput<UtxoAccumulatorOutput<Self>>
        + HasInput<Utxo<Self>>
        + HasInput<Note<Self>>
        + HasInput<Nullifier<Self>>;
}

/// Compiler Type
pub type Compiler<C> = <C as Configuration>::Compiler;

/// Proof System Type
type ProofSystemType<C> = <C as Configuration>::ProofSystem;

/// Proof System Error Type
pub type ProofSystemError<C> = <ProofSystemType<C> as ProofSystem>::Error;

/// Proof System Public Parameters Type
pub type ProofSystemPublicParameters<C> = <ProofSystemType<C> as ProofSystem>::PublicParameters;

/// Proving Context Type
pub type ProvingContext<C> = <ProofSystemType<C> as ProofSystem>::ProvingContext;

/// Verifying Context Type
pub type VerifyingContext<C> = <ProofSystemType<C> as ProofSystem>::VerifyingContext;

/// Proof System Input Type
pub type ProofInput<C> = <ProofSystemType<C> as ProofSystem>::Input;

/// Validity Proof Type
pub type Proof<C> = <ProofSystemType<C> as ProofSystem>::Proof;

/// Parameters Type
pub type Parameters<C> = <C as Configuration>::Parameters;

/// Parameters Variable Type
pub type ParametersVar<C> = <C as Configuration>::ParametersVar;

/// Full Parameters Type
pub type FullParameters<'p, C> = utxo::FullParameters<'p, Parameters<C>>;

/// Full Parameters Variable Type
pub type FullParametersVar<'p, C> = utxo::FullParameters<'p, ParametersVar<C>, Compiler<C>>;

/// Full Parameters Reference Type
pub type FullParametersRef<'p, C> = utxo::FullParametersRef<'p, Parameters<C>>;

/// Full Parameters Reference Variable Type
pub type FullParametersRefVar<'p, C> = utxo::FullParametersRef<'p, ParametersVar<C>, Compiler<C>>;

/// UTXO Accumulator Model Type
pub type UtxoAccumulatorModel<C> = utxo::UtxoAccumulatorModel<Parameters<C>>;

/// UTXO Accumulator Model Variable Type
pub type UtxoAccumulatorModelVar<C> = utxo::UtxoAccumulatorModel<ParametersVar<C>, Compiler<C>>;

/// UTXO Accumulator Item Type
pub type UtxoAccumulatorItem<C> = utxo::UtxoAccumulatorItem<Parameters<C>>;

/// UTXO Accumulator Witness Type
pub type UtxoAccumulatorWitness<C> = utxo::UtxoAccumulatorWitness<Parameters<C>>;

/// UTXO Accumulator Output Type
pub type UtxoAccumulatorOutput<C> = utxo::UtxoAccumulatorOutput<Parameters<C>>;

/// UTXO Membership Proof Type
pub type UtxoMembershipProof<C> = MembershipProof<UtxoAccumulatorModel<C>>;

/// Address Type
pub type Address<C> = utxo::Address<Parameters<C>>;

/// Asset Id Type
pub type AssetId<C> = <C as Configuration>::AssetId;

/// Asset Value Type
pub type AssetValue<C> = <C as Configuration>::AssetValue;

/// Asset Type
pub type Asset<C> = asset::Asset<<C as Configuration>::AssetId, <C as Configuration>::AssetValue>;

/// Asset Variable Type
pub type AssetVar<C> =
    asset::Asset<<C as Configuration>::AssetIdVar, <C as Configuration>::AssetValueVar>;

/// Associated Data Type
pub type AssociatedData<C> = utxo::AssociatedData<Parameters<C>>;

/// Spending Key Type
pub type SpendingKey<C> = auth::SpendingKey<Parameters<C>>;

/// Authorization Context Type
pub type AuthorizationContext<C> = auth::AuthorizationContext<Parameters<C>>;

/// Authorization Context Variable Type
pub type AuthorizationContextVar<C> = auth::AuthorizationContext<ParametersVar<C>>;

/// Authorization Proof Type
pub type AuthorizationProof<C> = auth::AuthorizationProof<Parameters<C>>;

/// Authorization Proof Variable Type
pub type AuthorizationProofVar<C> = auth::AuthorizationProof<ParametersVar<C>>;

/// Authorization Type
pub type Authorization<C> = auth::Authorization<Parameters<C>>;

/// Authorization Variable Type
pub type AuthorizationVar<C> = auth::Authorization<ParametersVar<C>>;

/// Authorization Key Type
pub type AuthorizationKey<C> = auth::AuthorizationKey<Parameters<C>>;

/// Authorization Signature Type
pub type AuthorizationSignature<C> = auth::AuthorizationSignature<Parameters<C>>;

/// Unspent Transaction Output Type
pub type Utxo<C> = utxo::Utxo<Parameters<C>>;

/// Base Note Type
pub type BaseNote<C> = Var<<C as Configuration>::NoteVar, Public, <C as Configuration>::Compiler>;

/// Note Type
pub type Note<C> = utxo::Note<Parameters<C>>;

/// Nullifier Type
pub type Nullifier<C> = utxo::Nullifier<Parameters<C>>;

/// Identifier Type
pub type Identifier<C> = utxo::Identifier<Parameters<C>>;

/// Identified Asset Type
pub type IdentifiedAsset<C> = utxo::IdentifiedAsset<Parameters<C>>;

/// Pre-Sender Type
pub type PreSender<C> = sender::PreSender<Parameters<C>>;

/// Sender Type
pub type Sender<C> = sender::Sender<Parameters<C>>;

/// Sender Variable Type
pub type SenderVar<C> = sender::Sender<ParametersVar<C>, Compiler<C>>;

/// Sender Post Type
pub type SenderPost<C> = sender::SenderPost<Parameters<C>>;

/// Receiver Type
pub type Receiver<C> = receiver::Receiver<Parameters<C>>;

/// Receiver Variable Type
pub type ReceiverVar<C> = receiver::Receiver<ParametersVar<C>, Compiler<C>>;

/// Receiver Post Type
pub type ReceiverPost<C> = receiver::ReceiverPost<Parameters<C>>;

/// Generates an internal pair for `asset` against `authorization_context`.
#[inline]
pub fn internal_pair<C, R>(
    parameters: &Parameters<C>,
    authorization_context: &mut AuthorizationContext<C>,
    address: Address<C>,
    asset: Asset<C>,
    associated_data: AssociatedData<C>,
    rng: &mut R,
) -> (Receiver<C>, PreSender<C>)
where
    C: Configuration,
    R: CryptoRng + RngCore + ?Sized,
{
    let receiver = Receiver::<C>::sample(parameters, address, asset.clone(), associated_data, rng);
    let pre_sender = PreSender::<C>::sample(
        parameters,
        authorization_context,
        receiver.identifier(),
        asset,
        rng,
    );
    (receiver, pre_sender)
}

/// Generates an internal pair for a zero-asset with the given `asset_id` against
/// `authorization_context`.
#[inline]
pub fn internal_zero_pair<C, R>(
    parameters: &Parameters<C>,
    authorization_context: &mut AuthorizationContext<C>,
    address: Address<C>,
    asset_id: C::AssetId,
    associated_data: AssociatedData<C>,
    rng: &mut R,
) -> (Receiver<C>, PreSender<C>)
where
    C: Configuration,
    R: CryptoRng + RngCore + ?Sized,
{
    internal_pair::<C, R>(
        parameters,
        authorization_context,
        address,
        Asset::<C>::zero(asset_id),
        associated_data,
        rng,
    )
}

/// Transfer
#[derive(derivative::Derivative)]
#[derivative(
    Clone(bound = r"
        Authorization<C>: Clone,
        C::AssetId: Clone,
        C::AssetValue: Clone,
        Sender<C>: Clone,
        Receiver<C>: Clone"),
    Copy(bound = r"
        Authorization<C>: Copy,
        C::AssetId: Copy,
        C::AssetValue: Copy,
        Sender<C>: Copy,
        Receiver<C>: Copy"),
    Debug(bound = r"
        Authorization<C>: Debug,
        C::AssetId: Debug,
        C::AssetValue: Debug,
        Sender<C>: Debug,
        Receiver<C>: Debug"),
    Eq(bound = r"
        Authorization<C>: Eq,
        C::AssetId: Eq,
        C::AssetValue: Eq,
        Sender<C>: Eq,
        Receiver<C>: Eq"),
    Hash(bound = r"
        Authorization<C>: Hash,
        C::AssetId: Hash,
        C::AssetValue: Hash,
        Sender<C>: Hash,
        Receiver<C>: Hash"),
    PartialEq(bound = r"
        Authorization<C>: PartialEq,
        C::AssetId: PartialEq,
        C::AssetValue: PartialEq,
        Sender<C>: PartialEq,
        Receiver<C>: PartialEq")
)]
pub struct Transfer<
    C,
    const SOURCES: usize,
    const SENDERS: usize,
    const RECEIVERS: usize,
    const SINKS: usize,
> where
    C: Configuration,
{
    /// Authorization
    authorization: Option<Authorization<C>>,

    /// Asset Id
    asset_id: Option<C::AssetId>,

    /// Sources
    sources: [C::AssetValue; SOURCES],

    /// Senders
    senders: [Sender<C>; SENDERS],

    /// Receivers
    receivers: [Receiver<C>; RECEIVERS],

    /// Sinks
    sinks: [C::AssetValue; SINKS],
}

impl<C, const SOURCES: usize, const SENDERS: usize, const RECEIVERS: usize, const SINKS: usize>
    Transfer<C, SOURCES, SENDERS, RECEIVERS, SINKS>
where
    C: Configuration,
{
    /// Builds a new [`Transfer`] from its component parts.
    #[inline]
    pub fn new(
        authorization: impl Into<Option<Authorization<C>>>,
        asset_id: impl Into<Option<C::AssetId>>,
        sources: [C::AssetValue; SOURCES],
        senders: [Sender<C>; SENDERS],
        receivers: [Receiver<C>; RECEIVERS],
        sinks: [C::AssetValue; SINKS],
    ) -> Self {
        let authorization = authorization.into();
        let asset_id = asset_id.into();
        Self::check_shape(authorization.is_some(), asset_id.is_some());
        Self::new_unchecked(authorization, asset_id, sources, senders, receivers, sinks)
    }

    /// Checks that the [`Transfer`] has a valid shape.
    #[inline]
    pub fn check_shape(has_authorization: bool, has_visible_asset_id: bool) {
        Self::has_nonempty_input_shape();
        Self::has_nonempty_output_shape();
        Self::has_authorization_when_required(has_authorization);
        Self::has_visible_asset_id_when_required(has_visible_asset_id);
    }

    /// Checks that the input side of the transfer is not empty.
    #[inline]
    pub fn has_nonempty_input_shape() {
        assert_ne!(
            SOURCES + SENDERS,
            0,
            "Not enough participants on the input side."
        );
    }

    /// Checks that the output side of the transfer is not empty.
    #[inline]
    pub fn has_nonempty_output_shape() {
        assert_ne!(
            RECEIVERS + SINKS,
            0,
            "Not enough participants on the output side."
        );
    }

    /// Checks that the given `authorization` for [`Transfer`] building is present exactly when
    /// required.
    #[inline]
    pub fn has_authorization_when_required(has_authorization: bool) {
        if requires_authorization(SENDERS) {
            assert!(has_authorization, "Missing authorization when required.");
        } else {
            assert!(!has_authorization, "Given authorization when not required.");
        }
    }

    /// Checks that the given `asset_id` for [`Transfer`] building is visible exactly when required.
    #[inline]
    pub fn has_visible_asset_id_when_required(has_visible_asset_id: bool) {
        if has_public_participants(SOURCES, SINKS) {
            assert!(
                has_visible_asset_id,
                "Missing public asset id when required."
            );
        } else {
            assert!(
                !has_visible_asset_id,
                "Given public asset id when not required."
            );
        }
    }

    /// Builds a new [`Transfer`] without checking the number of participants on the input and
    /// output sides.
    #[inline]
    fn new_unchecked(
        authorization: Option<Authorization<C>>,
        asset_id: Option<C::AssetId>,
        sources: [C::AssetValue; SOURCES],
        senders: [Sender<C>; SENDERS],
        receivers: [Receiver<C>; RECEIVERS],
        sinks: [C::AssetValue; SINKS],
    ) -> Self {
        Self {
            authorization,
            asset_id,
            sources,
            senders,
            receivers,
            sinks,
        }
    }

    /// Constructs an [`Asset`] against the `asset_id` of `self` and `value`.
    #[inline]
    fn construct_asset(&self, value: &C::AssetValue) -> Option<Asset<C>> {
        Some(Asset::<C>::new(self.asset_id.clone()?, value.clone()))
    }

    /// Returns the `k`-th source in the transfer.
    #[inline]
    pub fn source(&self, k: usize) -> Option<Asset<C>> {
        self.sources
            .get(k)
            .and_then(|value| self.construct_asset(value))
    }

    /// Returns the `k`-th sink in the transfer.
    #[inline]
    pub fn sink(&self, k: usize) -> Option<Asset<C>> {
        self.sinks
            .get(k)
            .and_then(|value| self.construct_asset(value))
    }

    /// Generates the public input for the [`Transfer`] validation proof.
    #[inline]
    pub fn generate_proof_input(&self) -> ProofInput<C> {
        let mut input = Default::default();
        self.extend(&mut input);
        input
    }

    /// Builds a constraint system which asserts constraints against unknown variables.
    #[inline]
    pub fn unknown_constraints(parameters: FullParametersRef<C>) -> C::Compiler {
        let mut compiler = C::ProofSystem::context_compiler();
        TransferVar::<C, SOURCES, SENDERS, RECEIVERS, SINKS>::new_unknown(&mut compiler)
            .build_validity_constraints(&parameters.as_constant(&mut compiler), &mut compiler);
        compiler
    }

    /// Builds a constraint system which asserts constraints against known variables.
    #[inline]
    pub fn known_constraints(&self, parameters: FullParametersRef<C>) -> C::Compiler {
        let mut compiler = C::ProofSystem::proof_compiler();
        let transfer: TransferVar<C, SOURCES, SENDERS, RECEIVERS, SINKS> =
            self.as_known(&mut compiler);
        transfer.build_validity_constraints(&parameters.as_constant(&mut compiler), &mut compiler);
        compiler
    }

    /// Generates a proving and verifying context for this transfer shape.
    #[inline]
    pub fn generate_context<R>(
        public_parameters: &ProofSystemPublicParameters<C>,
        parameters: FullParametersRef<C>,
        rng: &mut R,
    ) -> Result<(ProvingContext<C>, VerifyingContext<C>), ProofSystemError<C>>
    where
        R: CryptoRng + RngCore + ?Sized,
    {
        C::ProofSystem::compile(
            public_parameters,
            Self::unknown_constraints(parameters),
            rng,
        )
    }

    /// Converts `self` into its [`TransferPostBody`] by building the [`Transfer`] validity proof.
    #[allow(clippy::type_complexity)] // FIXME: Use a better abstraction here.
    #[inline]
    fn into_post_body_with_authorization<R>(
        self,
        parameters: FullParametersRef<C>,
        proving_context: &ProvingContext<C>,
        rng: &mut R,
    ) -> Result<(TransferPostBody<C>, Option<Authorization<C>>), ProofSystemError<C>>
    where
        R: CryptoRng + RngCore + ?Sized,
    {
        Ok((
            TransferPostBody::build(
                C::ProofSystem::prove(proving_context, self.known_constraints(parameters), rng)?,
                self.asset_id,
                self.sources,
                self.senders,
                self.receivers,
                self.sinks,
            ),
            self.authorization,
        ))
    }

    /// Converts `self` into its [`TransferPostBody`] by building the [`Transfer`] validity proof.
    #[inline]
    pub fn into_post_body<R>(
        self,
        parameters: FullParametersRef<C>,
        proving_context: &ProvingContext<C>,
        rng: &mut R,
    ) -> Result<TransferPostBody<C>, ProofSystemError<C>>
    where
        R: CryptoRng + RngCore + ?Sized,
    {
        Ok(self
            .into_post_body_with_authorization(parameters, proving_context, rng)?
            .0)
    }

    /// Converts `self` into its [`TransferPost`] by building the [`Transfer`] validity proof and
    /// signing the [`TransferPostBody`] payload.
    ///
    /// Returns `Ok(None)` when the authorization required by this [`Transfer`] is invalid or not
    /// provided. Returns `Err` when proof generation fails.
    #[inline]
    pub fn into_post<R>(
        self,
        parameters: FullParametersRef<C>,
        proving_context: &ProvingContext<C>,
        spending_key: Option<&SpendingKey<C>>,
        sink_accounts: Vec<C::AccountId>,
        rng: &mut R,
    ) -> Result<Option<TransferPost<C>>, ProofSystemError<C>>
    where
        R: CryptoRng + RngCore + ?Sized,
    {
        match (
            requires_authorization(SENDERS),
            self.authorization.is_some(),
            spending_key,
        ) {
            (true, true, Some(spending_key)) => {
                let (body, authorization) =
                    self.into_post_body_with_authorization(parameters, proving_context, rng)?;
                let body_with_accounts = BodyWithAccountsRef::new(&body, &sink_accounts);
                match auth::sign(
                    parameters.base,
                    spending_key,
                    authorization.expect("It is known to be `Some` from the check above."),
                    &body_with_accounts,
                    rng,
                ) {
                    Some(authorization_signature) => {
                        if has_sinks(SINKS) {
                            Ok(Some(TransferPost::new_unchecked_with_sinks(
                                Some(authorization_signature),
                                body,
                                sink_accounts,
                            )))
                        } else {
                            Ok(Some(TransferPost::new_unchecked(
                                Some(authorization_signature),
                                body,
                            )))
                        }
                    }
                    _ => Ok(None),
                }
            }
            (false, false, None) => Ok(Some(TransferPost::new_unchecked(
                None,
                self.into_post_body(parameters, proving_context, rng)?,
            ))),
            _ => Ok(None),
        }
    }
}

impl<C, const SOURCES: usize, const SENDERS: usize, const RECEIVERS: usize, const SINKS: usize>
    Input<C::ProofSystem> for Transfer<C, SOURCES, SENDERS, RECEIVERS, SINKS>
where
    C: Configuration,
{
    #[inline]
    fn extend(&self, input: &mut ProofInput<C>) {
        if let Some(authorization) = &self.authorization {
            C::ProofSystem::extend(input, Field::get(authorization))
        }
        if let Some(asset_id) = &self.asset_id {
            C::ProofSystem::extend(input, asset_id);
        }
        self.sources
            .iter()
            .for_each(|source| C::ProofSystem::extend(input, source));
        self.senders
            .iter()
            .for_each(|sender| C::ProofSystem::extend(input, sender));
        self.receivers
            .iter()
            .for_each(|receiver| C::ProofSystem::extend(input, receiver));
        self.sinks
            .iter()
            .for_each(|sink| C::ProofSystem::extend(input, sink));
    }
}

/// Transfer Variable
#[derive(derivative::Derivative)]
#[derivative(
    Clone(bound = r"
            AuthorizationVar<C>: Clone,
            C::AssetIdVar: Clone,
            C::AssetValueVar: Clone,
            SenderVar<C>: Clone,
            ReceiverVar<C>: Clone"),
    Debug(bound = r"
            AuthorizationVar<C>: Debug,
            C::AssetIdVar: Debug,
            C::AssetValueVar: Debug,
            SenderVar<C>: Debug,
            ReceiverVar<C>: Debug"),
    Eq(bound = r"
            AuthorizationVar<C>: Eq,
            C::AssetIdVar: Eq,
            C::AssetValueVar: Eq,
            SenderVar<C>: Eq,
            ReceiverVar<C>: Eq"),
    Hash(bound = r"
            AuthorizationVar<C>: Hash,
            C::AssetIdVar: Hash,
            C::AssetValueVar: Hash,
            SenderVar<C>: Hash,
            ReceiverVar<C>: Hash"),
    PartialEq(bound = r"
            AuthorizationVar<C>: PartialEq,
            C::AssetIdVar: PartialEq,
            C::AssetValueVar: PartialEq,
            SenderVar<C>: PartialEq,
            ReceiverVar<C>: PartialEq")
)]
struct TransferVar<
    C,
    const SOURCES: usize,
    const SENDERS: usize,
    const RECEIVERS: usize,
    const SINKS: usize,
> where
    C: Configuration,
{
    /// Authorization
    authorization: Option<AuthorizationVar<C>>,

    /// Asset Id
    asset_id: Option<C::AssetIdVar>,

    /// Sources
    sources: Vec<C::AssetValueVar>,

    /// Senders
    senders: Vec<SenderVar<C>>,

    /// Receivers
    receivers: Vec<ReceiverVar<C>>,

    /// Sinks
    sinks: Vec<C::AssetValueVar>,
}

impl<C, const SOURCES: usize, const SENDERS: usize, const RECEIVERS: usize, const SINKS: usize>
    TransferVar<C, SOURCES, SENDERS, RECEIVERS, SINKS>
where
    C: Configuration,
{
    /// Builds constraints for the [`Transfer`] validity proof.
    #[inline]
    fn build_validity_constraints(
        self,
        parameters: &FullParametersVar<C>,
        compiler: &mut C::Compiler,
    ) {
        let mut secret_asset_ids = Vec::with_capacity(SENDERS + RECEIVERS);
        let input_sum = Self::input_sum(
            parameters,
            &mut secret_asset_ids,
            self.authorization,
            self.senders,
            self.sources,
            compiler,
        );
        let output_sum = Self::output_sum(
            parameters,
            &mut secret_asset_ids,
            self.receivers,
            self.sinks,
            compiler,
        );
        compiler.assert_eq(&input_sum, &output_sum);
        match self.asset_id {
            Some(asset_id) => compiler.assert_all_eq_to_base(&asset_id, secret_asset_ids.iter()),
            _ => compiler.assert_all_eq(secret_asset_ids.iter()),
        }
    }

    /// Computes the sum over all the input assets, asserting that they are all well-formed.
    #[inline]
    fn input_sum(
        parameters: &FullParametersVar<C>,
        secret_asset_ids: &mut Vec<C::AssetIdVar>,
        authorization: Option<AuthorizationVar<C>>,
        senders: Vec<SenderVar<C>>,
        sources: Vec<C::AssetValueVar>,
        compiler: &mut C::Compiler,
    ) -> C::AssetValueVar {
        if let Some(mut authorization) = authorization {
            authorization.assert_authorized(&parameters.base, compiler);
            Self::value_sum(
                senders
                    .into_iter()
                    .map(|s| {
                        let asset = s.well_formed_asset(
                            &parameters.base,
                            &parameters.utxo_accumulator_model,
                            &mut authorization.context,
                            compiler,
                        );
                        secret_asset_ids.push(asset.id);
                        asset.value
                    })
                    .chain(sources)
                    .collect::<Vec<_>>(),
                compiler,
            )
        } else {
            Self::value_sum(sources, compiler)
        }
    }

    /// Computes the sum over all the output assets, asserting that they are all well-formed.
    #[inline]
    fn output_sum(
        parameters: &FullParametersVar<C>,
        secret_asset_ids: &mut Vec<C::AssetIdVar>,
        receivers: Vec<ReceiverVar<C>>,
        sinks: Vec<C::AssetValueVar>,
        compiler: &mut C::Compiler,
    ) -> C::AssetValueVar {
        Self::value_sum(
            receivers
                .into_iter()
                .map(|r| {
                    let asset = r.well_formed_asset(&parameters.base, compiler);
                    secret_asset_ids.push(asset.id);
                    asset.value
                })
                .chain(sinks)
                .collect::<Vec<_>>(),
            compiler,
        )
    }

    /// Computes the sum of the asset values over `iter`.
    #[inline]
    fn value_sum<I>(iter: I, compiler: &mut C::Compiler) -> C::AssetValueVar
    where
        I: IntoIterator<Item = C::AssetValueVar>,
    {
        iter.into_iter()
            .reduce(move |l, r| Add::add(l, r, compiler))
            .unwrap()
    }
}

impl<C, const SOURCES: usize, const SENDERS: usize, const RECEIVERS: usize, const SINKS: usize>
    Variable<Derived, C::Compiler> for TransferVar<C, SOURCES, SENDERS, RECEIVERS, SINKS>
where
    C: Configuration,
{
    type Type = Transfer<C, SOURCES, SENDERS, RECEIVERS, SINKS>;

    #[inline]
    fn new_unknown(compiler: &mut C::Compiler) -> Self {
        Self {
            authorization: requires_authorization(SENDERS).then(|| compiler.allocate_unknown()),
            asset_id: has_public_participants(SOURCES, SINKS)
                .then(|| compiler.allocate_unknown::<Public, _>()),
            sources: (0..SOURCES)
                .map(|_| compiler.allocate_unknown::<Public, _>())
                .collect(),
            senders: (0..SENDERS).map(|_| compiler.allocate_unknown()).collect(),
            receivers: (0..RECEIVERS)
                .map(|_| compiler.allocate_unknown())
                .collect(),
            sinks: (0..SINKS)
                .map(|_| compiler.allocate_unknown::<Public, _>())
                .collect(),
        }
    }

    #[inline]
    fn new_known(this: &Self::Type, compiler: &mut C::Compiler) -> Self {
        Self {
            authorization: this
                .authorization
                .as_ref()
                .map(|authorization| authorization.as_known(compiler)),
            asset_id: this
                .asset_id
                .as_ref()
                .map(|id| id.as_known::<Public, _>(compiler)),
            sources: this
                .sources
                .iter()
                .map(|source| source.as_known::<Public, _>(compiler))
                .collect(),
            senders: this
                .senders
                .iter()
                .map(|sender| sender.as_known(compiler))
                .collect(),
            receivers: this
                .receivers
                .iter()
                .map(|receiver| receiver.as_known(compiler))
                .collect(),
            sinks: this
                .sinks
                .iter()
                .map(|sink| sink.as_known::<Public, _>(compiler))
                .collect(),
        }
    }
}

/// Transfer Ledger
///
/// This is the validation trait for ensuring that a particular instance of [`Transfer`] is valid
/// according to the ledger state. These methods are the minimum required for a ledger which accepts
/// the [`Transfer`] abstraction. This `trait` inherits from [`SenderLedger`] and [`ReceiverLedger`]
/// which validate the [`Sender`] and [`Receiver`] parts of any [`Transfer`]. See their
/// documentation for more.
pub trait TransferLedger<C>:
    SenderLedger<
        Parameters<C>,
        SuperPostingKey = (Self::ValidProof, TransferLedgerSuperPostingKey<C, Self>),
    > + ReceiverLedger<
        Parameters<C>,
        SuperPostingKey = (Self::ValidProof, TransferLedgerSuperPostingKey<C, Self>),
    >
where
    C: Configuration + ?Sized,
{
    /// Super Posting Key
    ///
    /// Type that allows super-traits of [`TransferLedger`] to customize posting key behavior.
    type SuperPostingKey: Copy;

    /// Ledger Event
    type Event;

    /// Valid [`AssetValue`](Configuration::AssetValue) for [`TransferPost`] Source
    ///
    /// # Safety
    ///
    /// This type must be restricted so that it can only be constructed by this implementation of
    /// [`TransferLedger`].
    type ValidSourceAccount: AsRef<C::AssetValue>;

    /// Valid [`AssetValue`](Configuration::AssetValue) for [`TransferPost`] Sink
    ///
    /// # Safety
    ///
    /// This type must be restricted so that it can only be constructed by this implementation of
    /// [`TransferLedger`].
    type ValidSinkAccount: AsRef<C::AssetValue>;

    /// Valid [`Proof`] Posting Key
    ///
    /// # Safety
    ///
    /// This type must be restricted so that it can only be constructed by this implementation
    /// of [`TransferLedger`]. This is to prevent that [`SenderPostingKey::post`] and
    /// [`ReceiverPostingKey::post`] are called before [`SenderPost::validate`],
    /// [`ReceiverPost::validate`], [`check_source_accounts`](Self::check_source_accounts),
    /// [`check_sink_accounts`](Self::check_sink_accounts) and [`is_valid`](Self::is_valid).
    type ValidProof: Copy;

    /// Error Type
    type Error: From<<Self as ReceiverLedger<Parameters<C>>>::Error>
        + From<<Self as SenderLedger<Parameters<C>>>::Error>
        + Into<
            TransferPostError<
                C,
                C::AccountId,
                <Self as SenderLedger<Parameters<C>>>::Error,
                <Self as ReceiverLedger<Parameters<C>>>::Error,
                <Self as TransferLedger<C>>::Error,
            >,
        >;

    /// Checks that the balances associated to the source accounts are sufficient to withdraw the
    /// amount given in `sources`.
    fn check_source_accounts<I>(
        &self,
        asset_id: &C::AssetId,
        sources: I,
    ) -> Result<Vec<Self::ValidSourceAccount>, InvalidSourceAccount<C, C::AccountId>>
    where
        I: Iterator<Item = (C::AccountId, C::AssetValue)>;

    /// Checks that the sink accounts exist and balance can be increased by the specified amounts.
    fn check_sink_accounts<I>(
        &self,
        asset_id: &C::AssetId,
        sinks: I,
    ) -> Result<Vec<Self::ValidSinkAccount>, InvalidSinkAccount<C, C::AccountId>>
    where
        I: Iterator<Item = (C::AccountId, C::AssetValue)>;

    /// Checks that the transfer proof stored in `posting_key` is valid.
    fn is_valid(
        &self,
        posting_key: TransferPostingKeyRef<C, Self>,
    ) -> Result<(Self::ValidProof, Self::Event), <Self as TransferLedger<C>>::Error>;

    /// Updates the public balances in the ledger, finishing the transaction.
    ///
    /// # Crypto Safety
    ///
    /// This method can only be called once we check that `proof` is a valid proof and that
    /// `senders` and `receivers` are valid participants in the transaction. See
    /// [`is_valid`](Self::is_valid) for more.
    fn update_public_balances(
        &mut self,
        super_key: &TransferLedgerSuperPostingKey<C, Self>,
        asset_id: C::AssetId,
        sources: Vec<SourcePostingKey<C, Self>>,
        sinks: Vec<SinkPostingKey<C, Self>>,
        proof: Self::ValidProof,
    ) -> Result<(), <Self as TransferLedger<C>>::Error>;
}

/// Transfer Source Posting Key Type
pub type SourcePostingKey<C, L> = <L as TransferLedger<C>>::ValidSourceAccount;

/// Transfer Sink Posting Key Type
pub type SinkPostingKey<C, L> = <L as TransferLedger<C>>::ValidSinkAccount;

/// Transfer Sender Posting Key Type
pub type SenderPostingKey<C, L> = sender::SenderPostingKey<Parameters<C>, L>;

/// Transfer Receiver Posting Key Type
pub type ReceiverPostingKey<C, L> = receiver::ReceiverPostingKey<Parameters<C>, L>;

/// Transfer Ledger Super Posting Key Type
pub type TransferLedgerSuperPostingKey<C, L> = <L as TransferLedger<C>>::SuperPostingKey;

/// Invalid Authorization Signature Error
#[cfg_attr(
    feature = "serde",
    derive(Deserialize, Serialize),
    serde(crate = "manta_util::serde", deny_unknown_fields)
)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum InvalidAuthorizationSignature {
    /// Invalid Authorization Signature Shape
    InvalidShape,

    /// Missing Signature
    MissingSignature,

    /// Bad Signature
    BadSignature,
}

/// Invalid Source Accounts
///
/// This `struct` is the error state of the [`TransferLedger::check_source_accounts`] method. See
/// its documentation for more.
#[cfg_attr(
    feature = "serde",
    derive(Deserialize, Serialize),
    serde(crate = "manta_util::serde", deny_unknown_fields)
)]
#[derive(derivative::Derivative)]
#[derivative(
    Clone(bound = "AccountId: Clone, C::AssetId: Clone, C::AssetValue: Clone"),
    Copy(bound = "AccountId: Copy, C::AssetId: Copy, C::AssetValue: Copy"),
    Debug(bound = "AccountId: Debug, C::AssetId: Debug, C::AssetValue: Debug"),
    Eq(bound = "AccountId: Eq, C::AssetId: Eq, C::AssetValue: Eq"),
    Hash(bound = "AccountId: Hash, C::AssetId: Hash, C::AssetValue: Hash"),
    PartialEq(bound = "AccountId: PartialEq, C::AssetId: PartialEq, C::AssetValue: PartialEq")
)]
pub struct InvalidSourceAccount<C, AccountId>
where
    C: Configuration + ?Sized,
{
    /// Account Id
    pub account_id: AccountId,

    /// Asset Id
    pub asset_id: C::AssetId,

    /// Amount Attempting to Withdraw
    pub withdraw: C::AssetValue,
}

/// Invalid Sink Accounts
///
/// This `struct` is the error state of the [`TransferLedger::check_sink_accounts`] method. See its
/// documentation for more.
#[cfg_attr(
    feature = "serde",
    derive(Deserialize, Serialize),
    serde(crate = "manta_util::serde", deny_unknown_fields)
)]
#[derive(derivative::Derivative)]
#[derivative(
    Clone(bound = "AccountId: Clone, C::AssetId: Clone, C::AssetValue: Clone"),
    Copy(bound = "AccountId: Copy, C::AssetId: Copy, C::AssetValue: Copy"),
    Debug(bound = "AccountId: Debug, C::AssetId: Debug, C::AssetValue: Debug"),
    Eq(bound = "AccountId: Eq, C::AssetId: Eq, C::AssetValue: Eq"),
    Hash(bound = "AccountId: Hash, C::AssetId: Hash, C::AssetValue: Hash"),
    PartialEq(bound = "AccountId: PartialEq, C::AssetId: PartialEq, C::AssetValue: PartialEq")
)]
pub struct InvalidSinkAccount<C, AccountId>
where
    C: Configuration + ?Sized,
{
    /// Account Id
    pub account_id: AccountId,

    /// Asset Id
    pub asset_id: C::AssetId,

    /// Amount Attempting to Deposit
    pub deposit: C::AssetValue,
}

/// Transfer Ledger Post Error
pub type TransferLedgerPostError<C, L> = TransferPostError<
    C,
    <C as Configuration>::AccountId,
    <L as SenderLedger<Parameters<C>>>::Error,
    <L as ReceiverLedger<Parameters<C>>>::Error,
    <L as TransferLedger<C>>::Error,
>;

/// Transfer Post Error
///
/// This `enum` is the error state of the [`TransferPost::validate`] method. See its documentation
/// for more.
#[cfg_attr(
    feature = "serde",
    derive(Deserialize, Serialize),
    serde(
        bound(
            deserialize = r"
                AccountId: Deserialize<'de>,
                SenderError: Deserialize<'de>,
                ReceiverError: Deserialize<'de>,
                Error: Deserialize<'de>,
                C::AssetId: Deserialize<'de>,
                C::AssetValue: Deserialize<'de>",
            serialize = r"
                AccountId: Serialize,
                SenderError: Serialize,
                ReceiverError: Serialize,
                Error: Serialize,
                C::AssetId: Serialize,
                C::AssetValue: Serialize",
        ),
        crate = "manta_util::serde",
        deny_unknown_fields
    )
)]
#[derive(derivative::Derivative)]
#[derivative(
    Clone(
        bound = "AccountId: Clone, SenderError: Clone, ReceiverError: Clone, Error: Clone, C::AssetId: Clone, C::AssetValue: Clone"
    ),
    Copy(
        bound = "AccountId: Copy, SenderError: Copy, ReceiverError: Copy, Error: Copy, C::AssetId: Copy, C::AssetValue: Copy"
    ),
    Debug(
        bound = "AccountId: Debug, SenderError: Debug, ReceiverError: Debug, Error: Debug, C::AssetId: Debug, C::AssetValue: Debug"
    ),
    Eq(
        bound = "AccountId: Eq, SenderError: Eq, ReceiverError: Eq, Error: Eq, C::AssetId: Eq, C::AssetValue: Eq"
    ),
    Hash(
        bound = "AccountId: Hash, SenderError: Hash, ReceiverError: Hash, Error: Hash, C::AssetId: Hash, C::AssetValue: Hash"
    ),
    PartialEq(
        bound = "AccountId: PartialEq, SenderError: PartialEq, ReceiverError: PartialEq, Error: PartialEq, C::AssetId: PartialEq, C::AssetValue: PartialEq"
    )
)]
pub enum TransferPostError<C, AccountId, SenderError, ReceiverError, Error>
where
    C: Configuration + ?Sized,
{
    /// Invalid Transfer Post Shape
    InvalidShape,

    /// Invalid Authorization Signature
    ///
    /// The authorization signature for the [`TransferPost`] was not valid.
    InvalidAuthorizationSignature(InvalidAuthorizationSignature),

    /// Invalid Source Accounts
    InvalidSourceAccount(InvalidSourceAccount<C, AccountId>),

    /// Invalid Sink Accounts
    InvalidSinkAccount(InvalidSinkAccount<C, AccountId>),

    /// Sender Post Error
    Sender(SenderPostError<SenderError>),

    /// Receiver Post Error
    Receiver(ReceiverPostError<ReceiverError>),

    /// Duplicate Spend Error
    DuplicateSpend,

    /// Duplicate Mint Error
    DuplicateMint,

    /// Invalid Transfer Proof Error
    ///
    /// Validity of the transfer could not be proved by the ledger.
    InvalidProof,

    /// Unexpected Error
    ///
    /// An unexpected error occured.
    UnexpectedError(Error),
}

impl<C, AccountId, SenderError, ReceiverError, Error> From<InvalidAuthorizationSignature>
    for TransferPostError<C, AccountId, SenderError, ReceiverError, Error>
where
    C: Configuration + ?Sized,
{
    #[inline]
    fn from(err: InvalidAuthorizationSignature) -> Self {
        Self::InvalidAuthorizationSignature(err)
    }
}

impl<C, AccountId, SenderError, ReceiverError, Error> From<InvalidSourceAccount<C, AccountId>>
    for TransferPostError<C, AccountId, SenderError, ReceiverError, Error>
where
    C: Configuration + ?Sized,
{
    #[inline]
    fn from(err: InvalidSourceAccount<C, AccountId>) -> Self {
        Self::InvalidSourceAccount(err)
    }
}

impl<C, AccountId, SenderError, ReceiverError, Error> From<InvalidSinkAccount<C, AccountId>>
    for TransferPostError<C, AccountId, SenderError, ReceiverError, Error>
where
    C: Configuration + ?Sized,
{
    #[inline]
    fn from(err: InvalidSinkAccount<C, AccountId>) -> Self {
        Self::InvalidSinkAccount(err)
    }
}

impl<C, AccountId, SenderError, ReceiverError, Error> From<SenderPostError<SenderError>>
    for TransferPostError<C, AccountId, SenderError, ReceiverError, Error>
where
    C: Configuration + ?Sized,
{
    #[inline]
    fn from(err: SenderPostError<SenderError>) -> Self {
        Self::Sender(err)
    }
}

impl<C, AccountId, SenderError, ReceiverError, Error> From<ReceiverPostError<ReceiverError>>
    for TransferPostError<C, AccountId, SenderError, ReceiverError, Error>
where
    C: Configuration + ?Sized,
{
    #[inline]
    fn from(err: ReceiverPostError<ReceiverError>) -> Self {
        Self::Receiver(err)
    }
}

/// Transfer Post Body
#[cfg_attr(
    feature = "serde",
    derive(Deserialize, Serialize),
    serde(
        bound(
            deserialize = r"
                C::AssetId: Deserialize<'de>,
                C::AssetValue: Deserialize<'de>,
                SenderPost<C>: Deserialize<'de>,
                ReceiverPost<C>: Deserialize<'de>,
                Proof<C>: Deserialize<'de>,
            ",
            serialize = r"
                C::AssetId: Serialize,
                C::AssetValue: Serialize,
                SenderPost<C>: Serialize,
                ReceiverPost<C>: Serialize,
                Proof<C>: Serialize,
            ",
        ),
        crate = "manta_util::serde",
        deny_unknown_fields
    )
)]
#[derive(derivative::Derivative)]
#[derivative(
    Clone(bound = r"
        C::AssetId: Clone,
        C::AssetValue: Clone,
        SenderPost<C>: Clone,
        ReceiverPost<C>: Clone,
        Proof<C>: Clone
    "),
    Debug(bound = r"
        C::AssetId: Debug,
        C::AssetValue: Debug,
        SenderPost<C>: Debug,
        ReceiverPost<C>: Debug,
        Proof<C>: Debug
    "),
    Eq(bound = r"
        C::AssetId: Eq,
        C::AssetValue: Eq,
        SenderPost<C>: Eq,
        ReceiverPost<C>: Eq,
        Proof<C>: Eq
    "),
    Hash(bound = r"
        C::AssetId: Hash,
        C::AssetValue: Hash,
        SenderPost<C>: Hash,
        ReceiverPost<C>: Hash,
        Proof<C>: Hash
    "),
    PartialEq(bound = r"
        C::AssetId: PartialEq,
        C::AssetValue: PartialEq,
        SenderPost<C>: PartialEq,
        ReceiverPost<C>: PartialEq,
        Proof<C>: PartialEq
    ")
)]
pub struct TransferPostBody<C>
where
    C: Configuration + ?Sized,
{
    /// Asset Id
    pub asset_id: Option<C::AssetId>,

    /// Sources
    pub sources: Vec<C::AssetValue>,

    /// Sender Posts
    pub sender_posts: Vec<SenderPost<C>>,

    /// Receiver Posts
    pub receiver_posts: Vec<ReceiverPost<C>>,

    /// Sinks
    pub sinks: Vec<C::AssetValue>,

    /// Proof
    pub proof: Proof<C>,
}

impl<C> TransferPostBody<C>
where
    C: Configuration + ?Sized,
{
    /// Builds a new [`TransferPostBody`].
    #[inline]
    fn build<
        const SOURCES: usize,
        const SENDERS: usize,
        const RECEIVERS: usize,
        const SINKS: usize,
    >(
        proof: Proof<C>,
        asset_id: Option<C::AssetId>,
        sources: [C::AssetValue; SOURCES],
        senders: [Sender<C>; SENDERS],
        receivers: [Receiver<C>; RECEIVERS],
        sinks: [C::AssetValue; SINKS],
    ) -> Self {
        Self {
            asset_id,
            sources: sources.into(),
            sender_posts: senders.into_iter().map(Sender::<C>::into_post).collect(),
            receiver_posts: receivers
                .into_iter()
                .map(Receiver::<C>::into_post)
                .collect(),
            sinks: sinks.into(),
            proof,
        }
    }

    /// Constructs an [`Asset`] against the `asset_id` of `self` and `value`.
    #[inline]
    fn construct_asset(&self, value: &C::AssetValue) -> Option<Asset<C>> {
        Some(Asset::<C>::new(self.asset_id.clone()?, value.clone()))
    }

    /// Returns the `k`-th source in the transfer.
    #[inline]
    pub fn source(&self, k: usize) -> Option<Asset<C>> {
        self.sources
            .get(k)
            .and_then(|value| self.construct_asset(value))
    }

    /// Returns the `k`-th sink in the transfer.
    #[inline]
    pub fn sink(&self, k: usize) -> Option<Asset<C>> {
        self.sinks
            .get(k)
            .and_then(|value| self.construct_asset(value))
    }
}

impl<C> Encode for TransferPostBody<C>
where
    C: Configuration + ?Sized,
    C::AssetId: Encode,
    C::AssetValue: Encode,
    SenderPost<C>: Encode,
    ReceiverPost<C>: Encode,
    Proof<C>: Encode,
{
    #[inline]
    fn encode<W>(&self, mut writer: W) -> Result<(), W::Error>
    where
        W: Write,
    {
        self.asset_id.encode(&mut writer)?;
        self.sources.encode(&mut writer)?;
        self.sender_posts.encode(&mut writer)?;
        self.receiver_posts.encode(&mut writer)?;
        self.sinks.encode(&mut writer)?;
        self.proof.encode(&mut writer)?;
        Ok(())
    }
}

impl<C> Input<C::ProofSystem> for TransferPostBody<C>
where
    C: Configuration + ?Sized,
{
    #[inline]
    fn extend(&self, input: &mut ProofInput<C>) {
        if let Some(asset_id) = &self.asset_id {
            C::ProofSystem::extend(input, asset_id);
        }
        self.sources
            .iter()
            .for_each(|source| C::ProofSystem::extend(input, source));
        self.sender_posts
            .iter()
            .for_each(|post| C::ProofSystem::extend(input, post));
        self.receiver_posts
            .iter()
            .for_each(|post| C::ProofSystem::extend(input, post));
        self.sinks
            .iter()
            .for_each(|sink| C::ProofSystem::extend(input, sink));
    }
}

/// Body With Accounts Reference
pub struct BodyWithAccountsRef<'p, C>
where
    C: Configuration + ?Sized,
{
    /// Transfer Post Body
    pub body: &'p TransferPostBody<C>,

    /// Sink Accounts
    pub sink_accounts: &'p Vec<C::AccountId>,
}

impl<'p, C> BodyWithAccountsRef<'p, C>
where
    C: Configuration + ?Sized,
{
    /// Builds a new [`BodyWithAccountsRef`] from `body` and `sink_accounts`.
    #[inline]
    pub fn new(body: &'p TransferPostBody<C>, sink_accounts: &'p Vec<C::AccountId>) -> Self {
        Self {
            body,
            sink_accounts,
        }
    }
}

impl<'p, C> Encode for BodyWithAccountsRef<'p, C>
where
    C: Configuration + ?Sized,
    TransferPostBody<C>: Encode,
    C::AccountId: Encode,
{
    #[inline]
    fn encode<W>(&self, mut writer: W) -> Result<(), W::Error>
    where
        W: Write,
    {
        self.body.encode(&mut writer)?;
        self.sink_accounts.encode(&mut writer)?;
        Ok(())
    }
}

/// Transfer Post
#[cfg_attr(
    feature = "serde",
    derive(Deserialize, Serialize),
    serde(
        bound(
            deserialize = r"
                AuthorizationSignature<C>: Deserialize<'de>,
                TransferPostBody<C>: Deserialize<'de>,
                C::AccountId: Deserialize<'de>,
            ",
            serialize = r"
                AuthorizationSignature<C>: Serialize,
                TransferPostBody<C>: Serialize,
                C::AccountId: Serialize
            ",
        ),
        crate = "manta_util::serde",
        deny_unknown_fields
    )
)]
#[derive(derivative::Derivative)]
#[derivative(
    Clone(
        bound = "AuthorizationSignature<C>: Clone, TransferPostBody<C>: Clone, C::AccountId: Clone"
    ),
    Debug(
        bound = "AuthorizationSignature<C>: Debug, TransferPostBody<C>: Debug, C::AccountId: Debug"
    ),
    Eq(bound = "AuthorizationSignature<C>: Eq, TransferPostBody<C>: Eq, C::AccountId: Eq"),
    Hash(bound = "AuthorizationSignature<C>: Hash, TransferPostBody<C>: Hash, C::AccountId: Hash"),
    PartialEq(
        bound = "AuthorizationSignature<C>: PartialEq, TransferPostBody<C>: PartialEq, C::AccountId: PartialEq"
    )
)]
pub struct TransferPost<C>
where
    C: Configuration + ?Sized,
{
    /// Authorization Signature
    pub authorization_signature: Option<AuthorizationSignature<C>>,

    /// Transfer Post Body
    pub body: TransferPostBody<C>,

    /// Sink Accounts
    pub sink_accounts: Vec<C::AccountId>,
}

impl<C> TransferPost<C>
where
    C: Configuration + ?Sized,
{
    /// Builds a new [`TransferPost`] without checking the consistency conditions between the `body`,
    /// the `authorization_signature` and the `sink_accounts`.
    #[inline]
    fn new_unchecked_with_sinks(
        authorization_signature: Option<AuthorizationSignature<C>>,
        body: TransferPostBody<C>,
        sink_accounts: Vec<C::AccountId>,
    ) -> Self {
        Self {
            authorization_signature,
            body,
            sink_accounts,
        }
    }

    /// Builds a new [`TransferPost`] without checking the consistency conditions between the `body`
    /// and the `authorization_signature`.
    #[inline]
    fn new_unchecked(
        authorization_signature: Option<AuthorizationSignature<C>>,
        body: TransferPostBody<C>,
    ) -> Self {
        Self::new_unchecked_with_sinks(authorization_signature, body, Vec::new())
    }

    /// Returns the `k`-th source in the transfer.
    #[inline]
    pub fn source(&self, k: usize) -> Option<Asset<C>> {
        self.body.source(k)
    }

    /// Returns the `k`-th sink in the transfer.
    #[inline]
    pub fn sink(&self, k: usize) -> Option<Asset<C>> {
        self.body.sink(k)
    }

    /// Generates the public input for the [`Transfer`] validation proof.
    #[inline]
    pub fn generate_proof_input(&self) -> ProofInput<C> {
        let mut input = Default::default();
        self.extend(&mut input);
        input
    }

    /// Verifies the validity proof of `self` according to the `verifying_context`.
    #[inline]
    pub fn has_valid_proof(
        &self,
        verifying_context: &VerifyingContext<C>,
    ) -> Result<bool, ProofSystemError<C>> {
        C::ProofSystem::verify(
            verifying_context,
            &self.generate_proof_input(),
            &self.body.proof,
        )
    }

    /// Asserts that `self` has a valid proof. See [`has_valid_proof`](Self::has_valid_proof) for
    /// more.
    #[inline]
    pub fn assert_valid_proof(&self, verifying_context: &VerifyingContext<C>) -> &Proof<C>
    where
        Self: Debug,
        ProofSystemError<C>: Debug,
    {
        assert!(
            self.has_valid_proof(verifying_context)
                .expect("Unable to verify proof."),
            "Invalid TransferPost: {self:?}.",
        );
        &self.body.proof
    }

    /// Verifies that the authorization signature for `self` is valid under the `parameters`.
    #[inline]
    pub fn has_valid_authorization_signature(
        &self,
        parameters: &C::Parameters,
    ) -> Result<(), InvalidAuthorizationSignature> {
        match (
            &self.authorization_signature,
            requires_authorization(self.body.sender_posts.len()),
        ) {
            (Some(authorization_signature), true) => {
                let body_with_accounts = BodyWithAccountsRef::new(&self.body, &self.sink_accounts);
                if authorization_signature.verify(parameters, &body_with_accounts) {
                    Ok(())
                } else {
                    Err(InvalidAuthorizationSignature::BadSignature)
                }
            }
            (Some(_), false) => Err(InvalidAuthorizationSignature::InvalidShape),
            (None, true) => Err(InvalidAuthorizationSignature::MissingSignature),
            (None, false) => Ok(()),
        }
    }

    /// Checks that the public participant data is well-formed and runs `ledger` validation on
    /// source and sink accounts.
    #[allow(clippy::type_complexity)] // FIXME: Use a better abstraction for this.
    #[inline]
    fn check_public_participants<L>(
        asset_id: &Option<C::AssetId>,
        source_accounts: Vec<C::AccountId>,
        source_values: Vec<C::AssetValue>,
        sink_accounts: Vec<C::AccountId>,
        sink_values: Vec<C::AssetValue>,
        ledger: &L,
    ) -> Result<(Vec<L::ValidSourceAccount>, Vec<L::ValidSinkAccount>), TransferLedgerPostError<C, L>>
    where
        L: TransferLedger<C>,
    {
        let sources = source_values.len();
        let sinks = sink_values.len();
        if has_public_participants(sources, sinks) != asset_id.is_some() {
            return Err(TransferPostError::InvalidShape);
        }
        if source_accounts.len() != sources {
            return Err(TransferPostError::InvalidShape);
        }
        if sink_accounts.len() != sinks {
            return Err(TransferPostError::InvalidShape);
        }
        let sources = if sources > 0 {
            ledger.check_source_accounts(
                asset_id.as_ref().unwrap(),
                source_accounts.into_iter().zip(source_values),
            )?
        } else {
            Vec::new()
        };
        let sinks = if sinks > 0 {
            ledger.check_sink_accounts(
                asset_id.as_ref().unwrap(),
                sink_accounts.into_iter().zip(sink_values),
            )?
        } else {
            Vec::new()
        };
        Ok((sources, sinks))
    }

    /// Validates `self` on the transfer `ledger`.
    #[allow(clippy::type_complexity)] // FIXME: Use a better abstraction for this.
    #[inline]
    pub fn validate<L>(
        self,
        parameters: &C::Parameters,
        ledger: &L,
        source_accounts: Vec<C::AccountId>,
        sink_accounts: Vec<C::AccountId>,
    ) -> Result<TransferPostingKey<C, L>, TransferLedgerPostError<C, L>>
    where
        L: TransferLedger<C>,
    {
        self.has_valid_authorization_signature(parameters)?;
        let (source_posting_keys, sink_posting_keys) = Self::check_public_participants(
            &self.body.asset_id,
            source_accounts,
            self.body.sources,
            sink_accounts,
            self.body.sinks,
            ledger,
        )?;
        if !all_unequal(&self.body.sender_posts, |p, q| {
            p.nullifier.is_related(&q.nullifier)
        }) {
            return Err(TransferPostError::DuplicateSpend);
        }
        if !all_unequal(&self.body.receiver_posts, |p, q| p.utxo.is_related(&q.utxo)) {
            return Err(TransferPostError::DuplicateMint);
        }
        let sender_posting_keys = self
            .body
            .sender_posts
            .into_iter()
            .map(move |s| s.validate(ledger))
            .collect::<Result<Vec<_>, _>>()?;
        let receiver_posting_keys = self
            .body
            .receiver_posts
            .into_iter()
            .map(move |r| r.validate(ledger))
            .collect::<Result<Vec<_>, _>>()?;
        let (proof, event) = ledger
            .is_valid(TransferPostingKeyRef {
                authorization_key: &self.authorization_signature.map(|s| s.authorization_key),
                asset_id: &self.body.asset_id,
                sources: &source_posting_keys,
                senders: &sender_posting_keys,
                receivers: &receiver_posting_keys,
                sinks: &sink_posting_keys,
                proof: self.body.proof,
            })
            .map_err(|x| x.into())?;
        Ok(TransferPostingKey {
            asset_id: self.body.asset_id,
            source_posting_keys,
            sender_posting_keys,
            receiver_posting_keys,
            sink_posting_keys,
            proof,
            event,
        })
    }

    /// Validates `self` on the transfer `ledger` and then posts the updated state to the `ledger`
    /// if validation succeeded.
    #[inline]
    pub fn post<L>(
        self,
        parameters: &C::Parameters,
        ledger: &mut L,
        super_key: &TransferLedgerSuperPostingKey<C, L>,
        source_accounts: Vec<C::AccountId>,
        sink_accounts: Vec<C::AccountId>,
    ) -> Result<L::Event, TransferLedgerPostError<C, L>>
    where
        L: TransferLedger<C>,
    {
        self.validate(parameters, ledger, source_accounts, sink_accounts)?
            .post(ledger, super_key)
            .map_err(TransferPostError::UnexpectedError)
    }
}

impl<C> Encode for TransferPost<C>
where
    C: Configuration + ?Sized,
    AuthorizationSignature<C>: Encode,
    TransferPostBody<C>: Encode,
{
    #[inline]
    fn encode<W>(&self, mut writer: W) -> Result<(), W::Error>
    where
        W: Write,
    {
        self.authorization_signature.encode(&mut writer)?;
        self.body.encode(&mut writer)?;
        Ok(())
    }
}

impl<C> Input<C::ProofSystem> for TransferPost<C>
where
    C: Configuration + ?Sized,
{
    #[inline]
    fn extend(&self, input: &mut ProofInput<C>) {
        if let Some(authorization_signature) = &self.authorization_signature {
            C::ProofSystem::extend(input, &authorization_signature.authorization_key);
        }
        self.body.extend(input);
    }
}

/// Transfer Posting Key
#[cfg_attr(
    feature = "serde",
    derive(Deserialize, Serialize),
    serde(
        bound(
            deserialize = r"
                C::AssetId: Deserialize<'de>,
                SourcePostingKey<C, L>: Deserialize<'de>,
                SenderPostingKey<C, L>: Deserialize<'de>,
                ReceiverPostingKey<C, L>: Deserialize<'de>,
                SinkPostingKey<C, L>: Deserialize<'de>,
                L::ValidProof: Deserialize<'de>,
                L::Event: Deserialize<'de>",
            serialize = r"
                C::AssetId: Serialize,
                SourcePostingKey<C, L>: Serialize,
                SenderPostingKey<C, L>: Serialize,
                ReceiverPostingKey<C, L>: Serialize,
                SinkPostingKey<C, L>: Serialize,
                L::ValidProof: Serialize,
                L::Event: Serialize",
        ),
        crate = "manta_util::serde",
        deny_unknown_fields
    )
)]
#[derive(derivative::Derivative)]
#[derivative(
    Clone(bound = r"
            C::AssetId: Clone,
            SourcePostingKey<C, L>: Clone,
            SenderPostingKey<C, L>: Clone,
            ReceiverPostingKey<C, L>: Clone,
            SinkPostingKey<C, L>: Clone,
            L::ValidProof: Clone,
            L::Event: Clone"),
    Debug(bound = r"
            C::AssetId: Debug,
            SourcePostingKey<C, L>: Debug,
            SenderPostingKey<C, L>: Debug,
            ReceiverPostingKey<C, L>: Debug,
            SinkPostingKey<C, L>: Debug,
            L::ValidProof: Debug,
            L::Event: Debug"),
    Default(bound = r"
            C::AssetId: Default,
            SourcePostingKey<C, L>: Default,
            SenderPostingKey<C, L>: Default,
            ReceiverPostingKey<C, L>: Default,
            SinkPostingKey<C, L>: Default,
            L::ValidProof: Default,
            L::Event: Default"),
    Eq(bound = r"
            C::AssetId: Eq,
            SourcePostingKey<C, L>: Eq,
            SenderPostingKey<C, L>: Eq,
            ReceiverPostingKey<C, L>: Eq,
            SinkPostingKey<C, L>: Eq,
            L::ValidProof: Eq,
            L::Event: Eq"),
    Hash(bound = r"
            C::AssetId: Hash,
            SourcePostingKey<C, L>: Hash,
            SenderPostingKey<C, L>: Hash,
            ReceiverPostingKey<C, L>: Hash,
            SinkPostingKey<C, L>: Hash,
            L::ValidProof: Hash,
            L::Event: Hash"),
    PartialEq(bound = r"
            C::AssetId: PartialEq,
            SourcePostingKey<C, L>: PartialEq,
            SenderPostingKey<C, L>: PartialEq,
            ReceiverPostingKey<C, L>: PartialEq,
            SinkPostingKey<C, L>: PartialEq,
            L::ValidProof: PartialEq,
            L::Event: PartialEq")
)]
pub struct TransferPostingKey<C, L>
where
    C: Configuration + ?Sized,
    L: TransferLedger<C>,
{
    /// Asset Id
    asset_id: Option<C::AssetId>,

    /// Source Posting Keys
    source_posting_keys: Vec<SourcePostingKey<C, L>>,

    /// Sender Posting Keys
    sender_posting_keys: Vec<SenderPostingKey<C, L>>,

    /// Receiver Posting Keys
    receiver_posting_keys: Vec<ReceiverPostingKey<C, L>>,

    /// Sink Posting Keys
    sink_posting_keys: Vec<SinkPostingKey<C, L>>,

    /// Proof Posting Key
    proof: L::ValidProof,

    /// Ledger Event
    event: L::Event,
}

impl<C, L> TransferPostingKey<C, L>
where
    C: Configuration + ?Sized,
    L: TransferLedger<C>,
{
    /// Posts `self` to the transfer `ledger`.
    ///
    /// # Crypto Safety
    ///
    /// This method assumes that posting `self` to `ledger` is atomic and cannot fail. See
    /// [`SenderLedger::spend`] and [`ReceiverLedger::register`] for more information on the
    /// contract for this method.
    #[inline]
    pub fn post(
        self,
        ledger: &mut L,
        super_key: &TransferLedgerSuperPostingKey<C, L>,
    ) -> Result<L::Event, <L as TransferLedger<C>>::Error> {
        let proof = self.proof;
        SenderPostingKey::<C, _>::post_all(self.sender_posting_keys, ledger, &(proof, *super_key))?;
        ReceiverPostingKey::<C, _>::post_all(
            self.receiver_posting_keys,
            ledger,
            &(proof, *super_key),
        )?;
        if let Some(asset_id) = self.asset_id {
            ledger.update_public_balances(
                super_key,
                asset_id,
                self.source_posting_keys,
                self.sink_posting_keys,
                proof,
            )?;
        }
        Ok(self.event)
    }
}

/// Transfer Posting Key Reference
#[derive(derivative::Derivative)]
#[derivative(
    Debug(bound = r"
        AuthorizationKey<C>: Debug,
        C::AssetId: Debug,
        SourcePostingKey<C, L>: Debug,
        SenderPostingKey<C, L>: Debug,
        ReceiverPostingKey<C, L>: Debug,
        SinkPostingKey<C, L>: Debug,
        Proof<C>: Debug"),
    Eq(bound = r"
        AuthorizationKey<C>: Eq,
        C::AssetId: Eq,
        SourcePostingKey<C, L>: Eq,
        SenderPostingKey<C, L>: Eq,
        ReceiverPostingKey<C, L>: Eq,
        SinkPostingKey<C, L>: Eq,
        Proof<C>: Eq"),
    Hash(bound = r"
        AuthorizationKey<C>: Hash,
        C::AssetId: Hash,
        SourcePostingKey<C, L>: Hash,
        SenderPostingKey<C, L>: Hash,
        ReceiverPostingKey<C, L>: Hash,
        SinkPostingKey<C, L>: Hash,
        Proof<C>: Hash"),
    PartialEq(bound = r"
        AuthorizationKey<C>: PartialEq,
        C::AssetId: PartialEq,
        SourcePostingKey<C, L>: PartialEq,
        SenderPostingKey<C, L>: PartialEq,
        ReceiverPostingKey<C, L>: PartialEq,
        SinkPostingKey<C, L>: PartialEq,
        Proof<C>: PartialEq")
)]
pub struct TransferPostingKeyRef<'k, C, L>
where
    C: Configuration + ?Sized,
    L: TransferLedger<C> + ?Sized,
{
    /// Authorization Key
    pub authorization_key: &'k Option<AuthorizationKey<C>>,

    /// Asset Id
    pub asset_id: &'k Option<C::AssetId>,

    /// Sources
    pub sources: &'k [SourcePostingKey<C, L>],

    /// Senders
    pub senders: &'k [SenderPostingKey<C, L>],

    /// Receivers
    pub receivers: &'k [ReceiverPostingKey<C, L>],

    /// Sinks
    pub sinks: &'k [SinkPostingKey<C, L>],

    /// Proof
    pub proof: Proof<C>,
}

impl<'k, C, L> TransferPostingKeyRef<'k, C, L>
where
    C: Configuration + ?Sized,
    L: TransferLedger<C> + ?Sized,
{
    /// Generates the public input for the [`Transfer`] validation proof.
    #[inline]
    pub fn generate_proof_input(&self) -> ProofInput<C> {
        let mut input = Default::default();
        self.extend(&mut input);
        input
    }

    /// Verifies the validity proof of `self` according to the `verifying_context`.
    #[inline]
    pub fn has_valid_proof(
        &self,
        verifying_context: &VerifyingContext<C>,
    ) -> Result<bool, ProofSystemError<C>> {
        C::ProofSystem::verify(verifying_context, &self.generate_proof_input(), &self.proof)
    }
}

impl<'k, C, L> Input<C::ProofSystem> for TransferPostingKeyRef<'k, C, L>
where
    C: Configuration + ?Sized,
    L: TransferLedger<C> + ?Sized,
{
    #[inline]
    fn extend(&self, input: &mut ProofInput<C>) {
        if let Some(authorization_key) = &self.authorization_key {
            C::ProofSystem::extend(input, authorization_key);
        }
        if let Some(asset_id) = &self.asset_id {
            C::ProofSystem::extend(input, asset_id);
        }
        self.sources
            .iter()
            .for_each(|source| C::ProofSystem::extend(input, source.as_ref()));
        self.senders
            .iter()
            .for_each(|post| C::ProofSystem::extend(input, post));
        self.receivers
            .iter()
            .for_each(|post| C::ProofSystem::extend(input, post));
        self.sinks
            .iter()
            .for_each(|sink| C::ProofSystem::extend(input, sink.as_ref()));
    }
}

/// Identity Verification Error
#[cfg_attr(
    feature = "serde",
    derive(Deserialize, Serialize),
    serde(crate = "manta_util::serde", deny_unknown_fields)
)]
#[derive(derivative::Derivative)]
#[derivative(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum IdentityVerificationError {
    /// Invalid Signature
    InvalidSignature,

    /// Invalid Shape
    InvalidShape,

    /// Invalid Proof
    InvalidProof,

    /// Inconsistent Utxo Accumulator Output
    InconsistentUtxoAccumulatorOutput,

    /// Invalid Identified Asset
    InvalidVirtualAsset,

    /// Invalid Sink Account
    InvalidSinkAccount,
}

/// Identity Proof
///
/// # Note
///
/// To verify your identity, i.e., that you have the spending rights of a certain
/// [`Address`], you are prompted to generate a valid ToPublic [`TransferPost`]
/// spending a virtual [`IdentifiedAsset`].
#[cfg_attr(
    feature = "serde",
    derive(Deserialize, Serialize),
    serde(
        bound(
            deserialize = "TransferPost<C>: Deserialize<'de>, UtxoMembershipProof<C>: Deserialize<'de>",
            serialize = "TransferPost<C>: Serialize, UtxoMembershipProof<C>: Serialize",
        ),
        crate = "manta_util::serde",
        deny_unknown_fields
    )
)]
#[derive(derivative::Derivative)]
#[derivative(
    Clone(bound = "TransferPost<C>: Clone, UtxoMembershipProof<C>: Clone"),
    Copy(bound = "TransferPost<C>: Copy, UtxoMembershipProof<C>: Copy"),
    Debug(bound = "TransferPost<C>: Debug, UtxoMembershipProof<C>: Debug"),
    Eq(bound = "TransferPost<C>: Eq, UtxoMembershipProof<C>: Eq"),
    Hash(bound = "TransferPost<C>: Hash, UtxoMembershipProof<C>: Hash"),
    PartialEq(bound = "TransferPost<C>: PartialEq, UtxoMembershipProof<C>: PartialEq")
)]
pub struct IdentityProof<C>
where
    C: Configuration,
{
    /// TransferPost
    pub transfer_post: TransferPost<C>,
}

impl<C> IdentityProof<C>
where
    C: Configuration,
{
    /// Verifies `self` for `address` against `virtual_asset` and `public_account`.
    ///
    /// # Note
    ///
    /// It performs five checks:
    ///
    /// 1) `identity_proof.transfer_post` has a [`ToPublic`](crate::transfer::canonical::ToPublic)
    /// shape.
    ///
    /// 2) `identity_proof.transfer_post.sink_accounts` has only one element and is equal to
    /// `public_account`.
    ///
    /// 3) `identity_proof.transfer_post` has a valid authorization signature.
    ///
    /// 4) `identity_proof.transfer_post` has a valid proof.
    ///
    /// 5) The [`UtxoAccumulatorOutput`] in `identity_proof.transfer_post` has been computed from
    /// `virtual_asset` and `address`.
    #[inline]
    pub fn identity_verification<A>(
        &self,
        parameters: &Parameters<C>,
        verifying_context: &VerifyingContext<C>,
        utxo_accumulator_model: &UtxoAccumulatorModel<C>,
        virtual_asset: IdentifiedAsset<C>,
        address: Address<C>,
        public_account: C::AccountId,
    ) -> Result<(), IdentityVerificationError>
    where
        C::UtxoAccumulatorOutput: PartialEq,
        C::AccountId: PartialEq,
        UtxoAccumulatorModel<C>: Model<Verification = bool>,
        A: Accumulator<
            Item = UtxoAccumulatorItem<C>,
            Model = UtxoAccumulatorModel<C>,
            Output = UtxoAccumulatorOutput<C>,
        >,
        Asset<C>: Default,
    {
        TransferShape::from_post(&self.transfer_post).map_or(
            Err(IdentityVerificationError::InvalidShape),
            |shape| match shape {
                TransferShape::ToPublic => Ok(()),
                _ => Err(IdentityVerificationError::InvalidShape),
            },
        )?;
        if !self
            .transfer_post
            .sink_accounts
            .eq(&Vec::from([public_account]))
        {
            return Err(IdentityVerificationError::InvalidSinkAccount);
        }
        self.transfer_post
            .has_valid_authorization_signature(parameters)
            .map_err(|_| IdentityVerificationError::InvalidSignature)?;
        if !self
            .transfer_post
            .has_valid_proof(verifying_context)
            .map_err(|_| IdentityVerificationError::InvalidProof)?
        {
            return Err(IdentityVerificationError::InvalidProof);
        }
        let IdentifiedAsset::<C> { identifier, asset } = virtual_asset;
        let utxo = parameters.utxo_reconstruct(&asset, &identifier, &address);
        let mut utxo_accumulator = A::empty(utxo_accumulator_model);
        utxo_accumulator.insert(
            &parameters
                .utxo_accumulator_item_hash()
                .item_hash(&utxo, &mut ()),
        );
        let utxo_accumulator_output = utxo_accumulator
            .output_from(
                &parameters
                    .utxo_accumulator_item_hash()
                    .item_hash(&utxo, &mut ()),
            )
            .expect("Failed to obtain UtxoAccumulatorOutput");
        if !self
            .transfer_post
            .body
            .sender_posts
            .iter()
            .any(|sender_post| sender_post.utxo_accumulator_output == utxo_accumulator_output)
        {
            return Err(IdentityVerificationError::InconsistentUtxoAccumulatorOutput);
        }
        Ok(())
    }
}
