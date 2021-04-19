use ::{
    bitcoin::{network::constants::Network, secp256k1::Secp256k1, util::bip32 as secp_bip32},
    ed25519_dalek_bip32::{self as ed_bip32, ed25519_dalek as ed25519},
    solana_account_decoder::UiAccountData,
    solana_client::{rpc_client::RpcClient, rpc_request::TokenAccountsFilter},
    solana_sdk::{
        hash::Hash,
        message::Message,
        program_pack::Pack,
        pubkey::Pubkey,
        signature::{Keypair, Signer},
        system_instruction,
        transaction::Transaction,
    },
    spl_associated_token_account::*,
    std::{
        collections::{HashMap, HashSet},
        str::FromStr,
    },
};

fn main() {
    solana_logger::setup_with_default("off");
    let swap_accounts = false;

    let seed_phrase = "";
    let mnemonic = bip39::Mnemonic::from_phrase(&seed_phrase, bip39::Language::English).unwrap();
    let pass_phrase = "";
    let seed = bip39::Seed::new(&mnemonic, &pass_phrase);

    let secp256k1 = Secp256k1::new();
    let master =
        secp_bip32::ExtendedPrivKey::new_master(Network::Bitcoin, seed.as_bytes()).unwrap();
    let secp_path = "m/501'/0'/0/0";
    let derivation_path = secp_bip32::DerivationPath::from_str(&secp_path).unwrap();
    let derived = master.derive_priv(&secp256k1, &derivation_path).unwrap();
    let secret = ed25519::SecretKey::from_bytes(&derived.private_key.to_bytes()).unwrap();
    let public = ed25519::PublicKey::from(&secret);
    let degen_keypair = ed25519::Keypair { secret, public };

    let master = ed_bip32::ExtendedSecretKey::from_seed(seed.as_bytes()).unwrap();
    let ed_path = "m/44'/501'/0'/0'";
    let derivation_path = ed_bip32::DerivationPath::from_str(&ed_path).unwrap();
    let derived = master.derive(&derivation_path).unwrap();
    let secret = derived.secret_key;
    let public = ed25519::PublicKey::from(&secret);
    let keypair = ed25519::Keypair { secret, public };

    let (degen_keypair, keypair) = if swap_accounts {
        (keypair, degen_keypair)
    } else {
        (degen_keypair, keypair)
    };

    let degen_keypair = Keypair::from_bytes(&degen_keypair.to_bytes()).unwrap();
    let degen = degen_keypair.pubkey();
    let keypair = Keypair::from_bytes(&keypair.to_bytes()).unwrap();
    let pubkey = keypair.pubkey();

    let rpc_client = RpcClient::new("https://devnet.solana.com".to_string());

    let token_filter = TokenAccountsFilter::ProgramId(spl_token::id());
    let degen_token_accounts = rpc_client
        .get_token_accounts_by_owner(&degen, token_filter)
        .unwrap()
        .into_iter()
        .map(|a| {
            let address = Pubkey::from_str(&a.pubkey).unwrap();
            let (mint, amount) = match a.account.data {
                UiAccountData::Json(a) => {
                    let obj = a.parsed.as_object().unwrap();
                    let info = obj.get("info");
                    let mint = info
                        .and_then(|info| info.get("mint"))
                        .and_then(|mint| mint.as_str())
                        .and_then(|mint| Pubkey::from_str(mint).ok())
                        .unwrap();
                    let amount = info
                        .and_then(|info| info.get("tokenAmount"))
                        .and_then(|token_amount| token_amount.get("amount"))
                        .and_then(|amount| amount.as_str())
                        .and_then(|amount| u64::from_str(amount).ok())
                        .unwrap();
                    (mint, amount)
                }
                _ => panic!("rip"),
            };
            let ata = get_associated_token_address(&degen, &mint);
            let is_ata = ata == address;
            (mint, (address, amount, is_ata))
        })
        .collect::<HashMap<_, _>>();
    println!("degen: {:?}", degen_token_accounts);

    let token_filter = TokenAccountsFilter::ProgramId(spl_token::id());
    let token_accounts = rpc_client
        .get_token_accounts_by_owner(&pubkey, token_filter)
        .unwrap()
        .into_iter()
        .map(|a| {
            let address = Pubkey::from_str(&a.pubkey).unwrap();
            let (mint, amount) = match a.account.data {
                UiAccountData::Json(a) => {
                    let obj = a.parsed.as_object().unwrap();
                    let info = obj.get("info");
                    let mint = info
                        .and_then(|info| info.get("mint"))
                        .and_then(|mint| mint.as_str())
                        .and_then(|mint| Pubkey::from_str(mint).ok())
                        .unwrap();
                    let amount = info
                        .and_then(|info| info.get("tokenAmount"))
                        .and_then(|token_amount| token_amount.get("amount"))
                        .and_then(|amount| amount.as_str())
                        .and_then(|amount| u64::from_str(amount).ok())
                        .unwrap();
                    (mint, amount)
                }
                _ => panic!("rip"),
            };
            let ata = get_associated_token_address(&pubkey, &mint);
            let is_ata = ata == address;
            (mint, (address, amount, is_ata))
        })
        .collect::<HashMap<_, _>>();
    println!("pubkey: {:?}", token_accounts);

    let mut unique = HashSet::new();
    let mints = degen_token_accounts
        .keys()
        .filter(|address| unique.insert(*address))
        .cloned()
        .collect::<Vec<_>>();

    let mint_decimals = rpc_client
        .get_multiple_accounts(&mints)
        .unwrap()
        .into_iter()
        .zip(mints.iter())
        .filter_map(|(maybe_account, mint)| maybe_account.map(|account| (mint, account)))
        .map(|(mint, account)| {
            (
                mint,
                spl_token::state::Mint::unpack(&account.data)
                    .unwrap()
                    .decimals,
            )
        })
        .collect::<HashMap<_, _>>();
    println!("{:?}", mint_decimals);

    #[derive(Debug)]
    struct TransferMeta {
        src: Pubkey,
        dest: Pubkey,
        amount: u64,
        mint: Pubkey,
        create_dest: bool,
    }

    let mut need_create = false;
    let transfer_metas = degen_token_accounts.iter().fold(
        Vec::new(),
        |mut metas, (mint, (address, amount, _is_ata))| {
            let dest = token_accounts.get(mint);
            let (dest_address, create_dest) = if let Some(dest) = dest {
                (dest.0, false)
            } else {
                (get_associated_token_address(&pubkey, mint), true)
            };
            need_create |= create_dest;
            metas.push(TransferMeta {
                src: *address,
                dest: dest_address,
                amount: *amount,
                mint: *mint,
                create_dest,
            });
            metas
        },
    );
    println!("{:?}", transfer_metas);

    let mut instructions = transfer_metas
        .iter()
        .map(|tm| {
            let mut ix = Vec::new();
            if tm.create_dest {
                ix.push(create_associated_token_account(&degen, &pubkey, &tm.mint));
            }
            ix.push(
                spl_token::instruction::transfer_checked(
                    &spl_token::id(),
                    &tm.src,
                    &tm.mint,
                    &tm.dest,
                    &degen,
                    &[],
                    tm.amount,
                    mint_decimals[&tm.mint],
                )
                .unwrap(),
            );
            ix.push(
                spl_token::instruction::close_account(
                    &spl_token::id(),
                    &tm.src,
                    &degen,
                    &degen,
                    &[],
                )
                .unwrap(),
            );
            ix
        })
        .flatten()
        .collect::<Vec<_>>();
    println!("{:?}", instructions);

    let degen_balance = rpc_client.get_balance(&degen).unwrap();

    let recent_blockhash = if !instructions.is_empty() || degen_balance > 0 {
        let rent_balance = if need_create {
            rpc_client
                .get_minimum_balance_for_rent_exemption(spl_token::state::Account::LEN)
                .unwrap()
        } else {
            0
        };
        let (recent_blockhash, fee_calculator) = rpc_client.get_recent_blockhash().unwrap();
        let tmp_message = Message::new(&instructions, Some(&degen));
        let fee = fee_calculator.calculate_fee(&tmp_message);
        let amount = degen_balance.saturating_sub(fee);
        if amount < rent_balance {
            println!(
                "{} needs at least {} lamports to complete operation",
                degen,
                rent_balance + fee
            );
            std::process::exit(1);
        }
        if amount > 0 {
            instructions.push(system_instruction::transfer(&degen, &pubkey, amount));
        }
        recent_blockhash
    } else {
        Hash::default()
    };

    if !instructions.is_empty() {
        let message = Message::new(&instructions, Some(&degen));
        println!("{:?}", message);
        println!("{}", message.serialize().len());

        let mut transaction = Transaction::new_unsigned(message);

        transaction.sign(&[&degen_keypair], recent_blockhash);
        println!(
            "{}",
            rpc_client
                .send_and_confirm_transaction_with_spinner(&transaction)
                .unwrap()
        );
    } else {
        println!("nothing to do");
    }
}
