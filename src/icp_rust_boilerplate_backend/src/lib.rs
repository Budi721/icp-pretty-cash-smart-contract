#[macro_use]
extern crate serde;
use candid::{Decode, Encode};
use ic_cdk::api::time;
use ic_stable_structures::memory_manager::{MemoryId, MemoryManager, VirtualMemory};
use ic_stable_structures::{BoundedStorable, Cell, DefaultMemoryImpl, StableBTreeMap, Storable};
use std::{borrow::Cow, cell::RefCell};

type Memory = VirtualMemory<DefaultMemoryImpl>;
type IdCell = Cell<u64, Memory>;

#[derive(candid::CandidType, Clone, Serialize, Deserialize, Default)]
struct PettyCashEntry {
    id: u64,
    date: u64,
    description: String,
    amount: f64,
    entry_type: TransactionType,
    category: String,
    receipt_url: Option<String>,
    approved_by: Option<String>,
    created_at: u64,
    updated_at: Option<u64>,
}

#[derive(candid::CandidType, Clone, Serialize, Deserialize)]
enum TransactionType {
    Debit,  // Pengeluaran
    Credit, // Pengisian kas
}

impl Default for TransactionType {
    fn default() -> Self {
        TransactionType::Debit
    }
}

// Implementation for stable storage
impl Storable for PettyCashEntry {
    fn to_bytes(&self) -> std::borrow::Cow<[u8]> {
        Cow::Owned(Encode!(self).unwrap())
    }

    fn from_bytes(bytes: std::borrow::Cow<[u8]>) -> Self {
        Decode!(bytes.as_ref(), Self).unwrap()
    }
}

impl BoundedStorable for PettyCashEntry {
    const MAX_SIZE: u32 = 2048;
    const IS_FIXED_SIZE: bool = false;
}

thread_local! {
    static MEMORY_MANAGER: RefCell<MemoryManager<DefaultMemoryImpl>> = RefCell::new(
        MemoryManager::init(DefaultMemoryImpl::default())
    );

    static ID_COUNTER: RefCell<IdCell> = RefCell::new(
        IdCell::init(MEMORY_MANAGER.with(|m| m.borrow().get(MemoryId::new(0))), 0)
            .expect("Cannot create a counter")
    );

    static PETTY_CASH_STORAGE: RefCell<StableBTreeMap<u64, PettyCashEntry, Memory>> =
        RefCell::new(StableBTreeMap::init(
            MEMORY_MANAGER.with(|m| m.borrow().get(MemoryId::new(1)))
    ));

    static BALANCE: RefCell<Cell<f64, Memory>> = RefCell::new(
        Cell::init(MEMORY_MANAGER.with(|m| m.borrow().get(MemoryId::new(2))), 0.0)
            .expect("Cannot create balance cell")
    );
}

#[derive(candid::CandidType, Serialize, Deserialize)]
struct EntryPayload {
    description: String,
    amount: f64,
    entry_type: TransactionType,
    category: String,
    receipt_url: Option<String>,
    approved_by: Option<String>,
}

#[derive(candid::CandidType, Deserialize, Serialize)]
enum Error {
    NotFound { msg: String },
    InvalidAmount { msg: String },
    InsufficientFunds { msg: String },
}

// Query Methods
#[ic_cdk::query]
fn get_entry(id: u64) -> Result<PettyCashEntry, Error> {
    match _get_entry(&id) {
        Some(entry) => Ok(entry),
        None => Err(Error::NotFound {
            msg: format!("Entry with id={} not found", id),
        }),
    }
}

#[ic_cdk::query]
fn get_current_balance() -> f64 {
    BALANCE.with(|balance| *balance.borrow().get())
}

#[ic_cdk::query]
fn get_entries_by_date_range(start_date: u64, end_date: u64) -> Vec<PettyCashEntry> {
    PETTY_CASH_STORAGE.with(|storage| {
        storage
            .borrow()
            .iter()
            .filter(|(_, entry)| entry.date >= start_date && entry.date <= end_date)
            .map(|(_, entry)| entry)
            .collect()
    })
}

// Update Methods
#[ic_cdk::update]
fn add_entry(payload: EntryPayload) -> Result<PettyCashEntry, Error> {
    // Validate amount
    if payload.amount <= 0.0 {
        return Err(Error::InvalidAmount {
            msg: "Amount must be greater than 0".to_string(),
        });
    }

    // Check if sufficient funds for debit transactions
    if matches!(payload.entry_type, TransactionType::Debit) {
        let current_balance = get_current_balance();
        if current_balance < payload.amount {
            return Err(Error::InsufficientFunds {
                msg: format!(
                    "Insufficient funds. Current balance: {}, Required: {}",
                    current_balance, payload.amount
                ),
            });
        }
    }

    let id = ID_COUNTER
        .with(|counter| {
            let current_value = *counter.borrow().get();
            counter.borrow_mut().set(current_value + 1)
        })
        .expect("cannot increment id counter");

    let entry = PettyCashEntry {
        id,
        date: time(),
        description: payload.description,
        amount: payload.amount,
        entry_type: payload.entry_type.clone(),
        category: payload.category,
        receipt_url: payload.receipt_url,
        approved_by: payload.approved_by,
        created_at: time(),
        updated_at: None,
    };

    // Update balance
    let balance_change = match payload.entry_type {
        TransactionType::Credit => payload.amount,
        TransactionType::Debit => -payload.amount,
    };

    BALANCE.with(|balance| {
        let current_balance = *balance.borrow().get();
        balance
            .borrow_mut()
            .set(current_balance + balance_change)
            .expect("Cannot update balance");
    });

    do_insert(&entry);
    Ok(entry)
}

#[ic_cdk::update]
fn update_entry(id: u64, payload: EntryPayload) -> Result<PettyCashEntry, Error> {
    match PETTY_CASH_STORAGE.with(|service| service.borrow().get(&id)) {
        Some(mut entry) => {
            // Reverse the previous balance change
            let old_balance_change = match entry.entry_type {
                TransactionType::Credit => -entry.amount,
                TransactionType::Debit => entry.amount,
            };

            // Calculate new balance change
            let new_balance_change = match payload.entry_type {
                TransactionType::Credit => payload.amount,
                TransactionType::Debit => -payload.amount,
            };

            // Update balance
            BALANCE.with(|balance| {
                let current_balance = *balance.borrow().get();
                let new_balance = current_balance + old_balance_change + new_balance_change;
                
                if new_balance < 0.0 {
                    return Err(Error::InsufficientFunds {
                        msg: "Update would result in negative balance".to_string(),
                    });
                }

                balance
                    .borrow_mut()
                    .set(new_balance)
                    .expect("Cannot update balance");
                Ok(())
            })?;

            // Update entry fields
            entry.description = payload.description;
            entry.amount = payload.amount;
            entry.entry_type = payload.entry_type;
            entry.category = payload.category;
            entry.receipt_url = payload.receipt_url;
            entry.approved_by = payload.approved_by;
            entry.updated_at = Some(time());

            do_insert(&entry);
            Ok(entry)
        }
        None => Err(Error::NotFound {
            msg: format!("Entry with id={} not found", id),
        }),
    }
}

#[ic_cdk::update]
fn delete_entry(id: u64) -> Result<PettyCashEntry, Error> {
    match PETTY_CASH_STORAGE.with(|service| service.borrow_mut().remove(&id)) {
        Some(entry) => {
            // Update balance
            let balance_change = match entry.entry_type {
                TransactionType::Credit => -entry.amount,
                TransactionType::Debit => entry.amount,
            };

            BALANCE.with(|balance| {
                let current_balance = *balance.borrow().get();
                balance
                    .borrow_mut()
                    .set(current_balance + balance_change)
                    .expect("Cannot update balance");
            });

            Ok(entry)
        }
        None => Err(Error::NotFound {
            msg: format!("Entry with id={} not found", id),
        }),
    }
}

// Helper functions
fn do_insert(entry: &PettyCashEntry) {
    PETTY_CASH_STORAGE.with(|service| {
        service.borrow_mut().insert(entry.id, entry.clone())
    });
}

fn _get_entry(id: &u64) -> Option<PettyCashEntry> {
    PETTY_CASH_STORAGE.with(|service| service.borrow().get(id))
}

// Generate Candid interface
ic_cdk::export_candid!();